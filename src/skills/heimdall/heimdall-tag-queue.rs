//! heimdall-tag-queue.rs — twin of skills/heimdall/heimdall-tag-queue.mjs (db lane, cross-db
//! ATTACH: brain, async-over-sync). NAMES ARE CONTRACT: `heimdallTagQueue` (default). The
//! TRAINING tab's work stream: a deterministic queue of real calls awaiting a human verdict,
//! ranked by an all-SQL-derivable priority — burned +4, detector-rejected (degenerate/truncated/
//! validation_failed) +3, starving rung (no sample_ok row) +2, AI-6 active learning: low-confidence
//! AI verdict +3, AI/court disagreement +3, near-speaking rung (>=8 calls, not yet sample_ok) +1;
//! tiebreak newest first. card_slug present flips to SESSION REPLAY (chronological, a.id ASC).
//! Composes the already-converted casting twin (heimdall__helpers_casting::judgeEstTokens) for
//! the per-item AI-tagging cost estimate.
// QUALITY: 81/100 — dynamic WHERE/ORDER BY over 4 read queries + per-item A/B pair lookup

use rusqlite::Connection;
use serde_json::{json, Value};

// Standard helper functions (playbook §Standard helper functions) — copied verbatim (subset used).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}

const SAMPLE_NEED: i64 = 12;

fn count_n(db: &Connection, sql: &str) -> Result<Value, String> {
    let rows = crate::db_json::query_rows(db, sql, &[])?;
    Ok(rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).cloned().unwrap_or(json!(0)))
}

pub fn heimdallTagQueue(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing(...)` — the db lane
    // always injects a db, so this guard-throw is unreachable and not transcribed (gotcha 9).

    // `Math.min(Math.max(Number(input?.limit ?? 20), 1), 100)` — NaN propagates through both
    // Math.max/min exactly like a normal value would (neither call ever meets a NaN peer here
    // since 1 and 100 are literals), so a plain clamp suffices.
    let limit_input = nz(input.get("limit")).cloned().unwrap_or(json!(20));
    let limit_num = js_number(Some(&limit_input));
    let limit_num = if limit_num.is_nan() { f64::NAN } else { limit_num.max(1.0).min(100.0) };
    let limit_i64 = if limit_num.is_nan() { 0i64 } else { limit_num as i64 };

    // `input?.category ? String(input.category) : null` (truthiness, not `??`) for all three.
    let category = input.get("category").filter(|v| truthy(Some(v))).map(|v| js_string(v));
    let base = input.get("template_base").filter(|v| truthy(Some(v))).map(|v| js_string(v));
    let card_slug = input.get("card_slug").filter(|v| truthy(Some(v))).map(|v| js_string(v));

    let mut filters: Vec<&str> = Vec::new();
    let mut args: Vec<Value> = Vec::new();
    if let Some(c) = &category { filters.push("hc.category = ?"); args.push(json!(c)); }
    if let Some(b) = &base { filters.push("hc.template_base = ?"); args.push(json!(b)); }
    if let Some(cs) = &card_slug {
        filters.push("a.contract_slug IN (SELECT slug FROM contracts WHERE card_slug = ?)");
        args.push(json!(cs));
    }
    let filter_sql = if filters.is_empty() { String::new() } else { format!("AND {}", filters.join(" AND ")) };
    // card_slug present = SESSION REPLAY: chronological (oldest first); else priority-ranked.
    let order_sql = if card_slug.is_some() { "a.id ASC" } else { "priority DESC, a.id DESC" };

    let items_sql = format!(
        "SELECT a.id AS ai_call_id, hc.template_base, hc.category, a.tier, a.model_id,
               ct.card_slug, a.tokens_in, a.tokens_out,
               a.outcome_class, COALESCE(a.cost_usd_actual, 0) AS cost_usd, a.ts,
               EXISTS (SELECT 1 FROM brain.heimdall_evidence e WHERE e.ai_call_id = a.id AND e.outcome = 'burn') AS burned,
               NOT EXISTS (SELECT 1 FROM brain.heimdall_ledger l WHERE l.scope_kind = 'template' AND l.scope_ref = hc.template_base AND l.sample_ok = 1) AS starving,
               at.verdict AS ai_verdict, at.confidence AS ai_confidence, at.low_confidence AS ai_low_confidence,
               at.failure_class AS ai_failure_class,
               (at.verdict > 0 AND EXISTS (SELECT 1 FROM brain.heimdall_evidence e2 WHERE e2.ai_call_id = a.id AND e2.outcome = 'burn')) AS ai_court_disagree,
               (CASE WHEN EXISTS (SELECT 1 FROM brain.heimdall_evidence e WHERE e.ai_call_id = a.id AND e.outcome = 'burn') THEN 4 ELSE 0 END
                + CASE WHEN a.outcome_class IN ('degenerate','truncated','validation_failed') THEN 3 ELSE 0 END
                + CASE WHEN NOT EXISTS (SELECT 1 FROM brain.heimdall_ledger l WHERE l.scope_kind = 'template' AND l.scope_ref = hc.template_base AND l.sample_ok = 1) THEN 2 ELSE 0 END
                + CASE WHEN at.low_confidence = 1 THEN 3 ELSE 0 END
                + CASE WHEN at.verdict > 0 AND EXISTS (SELECT 1 FROM brain.heimdall_evidence e3 WHERE e3.ai_call_id = a.id AND e3.outcome = 'burn') THEN 3 ELSE 0 END
                + CASE WHEN EXISTS (SELECT 1 FROM brain.heimdall_ledger lb WHERE lb.scope_kind = 'template' AND lb.scope_ref = hc.template_base AND lb.sample_ok = 0 AND lb.calls >= 8) THEN 1 ELSE 0 END
               ) AS priority
        FROM brain.ai_calls a
        JOIN brain.heimdall_castings hc ON hc.ai_call_id = a.id
        LEFT JOIN brain.heimdall_ai_tags at ON at.target_ai_call_id = a.id AND at.superseded_by_human = 0
        LEFT JOIN contracts ct ON ct.slug = a.contract_slug
        WHERE a.cached = 0
          AND a.outcome_class IS NOT NULL AND a.outcome_class != 'provider_error'
          AND hc.template_base NOT LIKE 'heimdall.%'
          AND NOT EXISTS (SELECT 1 FROM brain.heimdall_human_tags t WHERE t.ai_call_id = a.id)
          AND NOT EXISTS (SELECT 1 FROM brain.heimdall_tag_skips s WHERE s.ai_call_id = a.id)
          {filter_sql}
        ORDER BY {order_sql}
        LIMIT ?"
    );
    let mut all_params = args.clone();
    all_params.push(json!(limit_i64));
    let rows = crate::db_json::query_rows(db, &items_sql, &all_params)?;
    let rows_arr = rows.as_array().cloned().unwrap_or_default();

    // `.map((r) => ({ ...r, burned: !!r.burned, starving: !!r.starving, ai_low_confidence: !!...,
    // ai_court_disagree: !!..., judge_est_tokens: judgeEstTokens({...}) }))` — spread keeps the
    // SELECT's column order; overriding an existing key keeps its position, judge_est_tokens is a
    // NEW key so it appends at the end (query_rows already builds the Map in SELECT column order).
    let mut items: Vec<Value> = Vec::new();
    for r in rows_arr {
        let mut obj = r.as_object().cloned().unwrap_or_default();
        let burned = truthy(obj.get("burned"));
        obj.insert("burned".into(), json!(burned));
        obj.insert("starving".into(), json!(truthy(obj.get("starving"))));
        obj.insert("ai_low_confidence".into(), json!(truthy(obj.get("ai_low_confidence"))));
        obj.insert("ai_court_disagree".into(), json!(truthy(obj.get("ai_court_disagree"))));
        let judge_input = json!({
            "tokens_in": obj.get("tokens_in").cloned().unwrap_or(Value::Null),
            "tokens_out": obj.get("tokens_out").cloned().unwrap_or(Value::Null),
            "panel": burned,
        });
        let jet = crate::heimdall__helpers_casting::judgeEstTokens(&judge_input);
        obj.insert("judge_est_tokens".into(), jet);
        items.push(Value::Object(obj));
    }

    // A/B pairs: an eligible sibling sharing the exact input_hash (same inputs, different
    // attempt/model). Deterministic: the lowest other id wins the pairing. MIN(...) is an
    // aggregate — it always yields exactly one row (pair = NULL when nothing matches), mirroring
    // the JS `pairStmt.get(...)?.pair ?? null` (never leaves the key unset).
    let pair_sql = "SELECT MIN(a2.id) AS pair FROM brain.ai_calls a2
        WHERE a2.input_hash = (SELECT input_hash FROM brain.ai_calls WHERE id = ?)
          AND a2.id != ? AND a2.cached = 0
          AND a2.outcome_class IS NOT NULL AND a2.outcome_class != 'provider_error'
          AND NOT EXISTS (SELECT 1 FROM brain.heimdall_human_tags t WHERE t.ai_call_id = a2.id)
          AND NOT EXISTS (SELECT 1 FROM brain.heimdall_tag_skips s WHERE s.ai_call_id = a2.id)";
    for item in items.iter_mut() {
        let ai_call_id = item.get("ai_call_id").cloned().unwrap_or(Value::Null);
        let pair_rows = crate::db_json::query_rows(db, pair_sql, &[ai_call_id.clone(), ai_call_id])?;
        let pair = pair_rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("pair")).cloned().unwrap_or(Value::Null);
        item.as_object_mut().unwrap().insert("pair_call_id".into(), pair);
    }

    // COVERAGE METER: per active category with traffic — best rung count vs the sample_ok floor.
    let cp_sql = "SELECT hc.category,
               COALESCE((SELECT MAX(l.calls) FROM brain.heimdall_ledger l
                         WHERE l.scope_kind = 'category' AND l.scope_ref = hc.category), 0) AS have,
               EXISTS (SELECT 1 FROM brain.heimdall_ledger l2
                       WHERE l2.scope_kind = 'category' AND l2.scope_ref = hc.category AND l2.sample_ok = 1) AS speaking
        FROM (SELECT DISTINCT category FROM brain.heimdall_castings) hc
        ORDER BY have DESC";
    let cp_rows = crate::db_json::query_rows(db, cp_sql, &[])?;
    let category_progress: Vec<Value> = cp_rows.as_array().cloned().unwrap_or_default().iter().map(|r| json!({
        "category": r.get("category").cloned().unwrap_or(Value::Null),
        "have": r.get("have").cloned().unwrap_or(Value::Null),
        "need": SAMPLE_NEED,
        "speaking": truthy(r.get("speaking")),
    })).collect();

    let progress = json!({
        "tagged": count_n(db, "SELECT COUNT(*) AS n FROM brain.heimdall_human_tags")?,
        "skipped": count_n(db, "SELECT COUNT(*) AS n FROM brain.heimdall_tag_skips")?,
        "remaining": count_n(db, "SELECT COUNT(*) AS n FROM brain.ai_calls a
            JOIN brain.heimdall_castings hc ON hc.ai_call_id = a.id
            WHERE a.cached = 0 AND a.outcome_class IS NOT NULL AND a.outcome_class != 'provider_error'
              AND hc.template_base NOT LIKE 'heimdall.%'
              AND NOT EXISTS (SELECT 1 FROM brain.heimdall_human_tags t WHERE t.ai_call_id = a.id)
              AND NOT EXISTS (SELECT 1 FROM brain.heimdall_tag_skips s WHERE s.ai_call_id = a.id)")?,
    });

    // SESSION LIST: every card with tagged castings, its untagged count, newest first.
    let runs_sql = "SELECT ct.card_slug,
               COUNT(*) AS calls,
               SUM(CASE WHEN NOT EXISTS (SELECT 1 FROM brain.heimdall_human_tags t WHERE t.ai_call_id = a.id)
                         AND NOT EXISTS (SELECT 1 FROM brain.heimdall_tag_skips s WHERE s.ai_call_id = a.id)
                        THEN 1 ELSE 0 END) AS untagged,
               MAX(a.ts) AS last_ts
        FROM brain.ai_calls a
        JOIN brain.heimdall_castings hc ON hc.ai_call_id = a.id
        JOIN contracts ct ON ct.slug = a.contract_slug
        WHERE a.cached = 0 AND a.contract_slug IS NOT NULL
        GROUP BY ct.card_slug
        ORDER BY last_ts DESC
        LIMIT 40";
    let runs = crate::db_json::query_rows(db, runs_sql, &[])?;

    Ok(json!({ "ok": true, "items": items, "progress": progress, "category_progress": category_progress, "runs": runs }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &[&str] = &[
        "CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, cached INTEGER NOT NULL DEFAULT 0, outcome_class TEXT, tier TEXT, model_id TEXT, contract_slug TEXT, tokens_in INTEGER, tokens_out INTEGER, cost_usd_actual REAL, ts TEXT, input_hash TEXT)",
        "CREATE TABLE brain.heimdall_castings (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, template_base TEXT, category TEXT)",
        "CREATE TABLE brain.heimdall_ai_tags (id INTEGER PRIMARY KEY AUTOINCREMENT, target_ai_call_id INTEGER, superseded_by_human INTEGER NOT NULL DEFAULT 0, verdict INTEGER, confidence REAL, low_confidence INTEGER, failure_class TEXT)",
        "CREATE TABLE brain.heimdall_evidence (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, outcome TEXT)",
        "CREATE TABLE brain.heimdall_ledger (model_slug TEXT, scope_kind TEXT, scope_ref TEXT, calls INTEGER, sample_ok INTEGER)",
        "CREATE TABLE brain.heimdall_human_tags (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER)",
        "CREATE TABLE brain.heimdall_tag_skips (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER)",
        "CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT)",
    ];

    fn schema(db: &Connection) {
        db.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        for s in SCHEMA { db.execute_batch(s).unwrap(); }
    }

    fn seeded_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        schema(&db);
        db.execute_batch("
            INSERT INTO brain.ai_calls (id,cached,outcome_class,tier,model_id,contract_slug,tokens_in,tokens_out,cost_usd_actual,ts,input_hash) VALUES
              (1,0,'ok','mid','claude-sonnet-4-6','CT-1',500,300,0.02,'2026-01-05T00:00:00.000Z','H1'),
              (2,0,'validation_failed','mid','gpt-5','CT-1',200,100,0.01,'2026-01-04T00:00:00.000Z','H2'),
              (3,0,'ok','mid','claude-sonnet-4-6','CT-2',100,50,NULL,'2026-01-03T00:00:00.000Z','HP'),
              (4,0,'ok','mid','gpt-5','CT-2',120,60,NULL,'2026-01-03T01:00:00.000Z','HP'),
              (5,1,'ok','mid','claude-sonnet-4-6','CT-1',10,10,0,'2026-01-02T00:00:00.000Z','H5'),
              (6,0,'provider_error','mid','claude-sonnet-4-6','CT-1',10,10,0,'2026-01-02T00:00:00.000Z','H6'),
              (7,0,NULL,'mid','claude-sonnet-4-6','CT-1',10,10,0,'2026-01-02T00:00:00.000Z','H7'),
              (8,0,'ok','mid','claude-sonnet-4-6','CT-1',10,10,0,'2026-01-02T00:00:00.000Z','H8'),
              (9,0,'ok','mid','claude-sonnet-4-6','CT-1',10,10,0,'2026-01-02T00:00:00.000Z','H9'),
              (10,0,'ok','mid','claude-sonnet-4-6','CT-1',10,10,0,'2026-01-02T00:00:00.000Z','H10'),
              (11,0,'ok','high','claude-opus-4-6','CT-3',1000,1000,0.05,'2026-01-06T00:00:00.000Z','H11');

            INSERT INTO brain.heimdall_castings (ai_call_id, template_base, category) VALUES
              (1,'chat','conversational'), (2,'summarize','writing'), (3,'chat','conversational'),
              (4,'chat','conversational'), (5,'chat','conversational'), (6,'chat','conversational'),
              (7,'chat','conversational'), (8,'heimdall.internal','conversational'), (9,'chat','conversational'),
              (10,'chat','conversational'), (11,'chat','conversational'), (99,'research-brief','research');

            INSERT INTO brain.heimdall_ai_tags (target_ai_call_id, superseded_by_human, verdict, confidence, low_confidence, failure_class) VALUES
              (1,1,1,0.5,0,NULL), (2,0,1,0.9,1,'hallucination'), (11,0,1,0.8,0,NULL);

            INSERT INTO brain.heimdall_evidence (ai_call_id, outcome) VALUES (1,'burn'), (11,'burn');

            INSERT INTO brain.heimdall_ledger (model_slug, scope_kind, scope_ref, calls, sample_ok) VALUES
              ('claude-sonnet-4-6','template','chat',10,0),
              ('gpt-5','template','summarize',20,1),
              ('claude-sonnet-4-6','category','conversational',25,1),
              ('gpt-5','category','writing',5,0);

            INSERT INTO brain.heimdall_human_tags (ai_call_id) VALUES (9);
            INSERT INTO brain.heimdall_tag_skips (ai_call_id) VALUES (10);

            INSERT INTO contracts (slug, card_slug) VALUES ('CT-1','CARD-A'), ('CT-2','CARD-B'), ('CT-3','CARD-C');
        ").unwrap();
        db
    }

    fn ids(v: &Value) -> Vec<i64> {
        v["items"].as_array().unwrap().iter().map(|i| i["ai_call_id"].as_i64().unwrap()).collect()
    }

    #[test]
    fn default_query_priority_flags_tokens_pairs_progress_runs() {
        let db = seeded_db();
        let r = heimdallTagQueue(&json!({}), &db).unwrap();
        assert_eq!(ids(&r), vec![11, 1, 2, 4, 3]);

        let by_id = |id: i64| r["items"].as_array().unwrap().iter().find(|i| i["ai_call_id"] == json!(id)).unwrap();
        let i11 = by_id(11);
        assert_eq!(i11["priority"], json!(10));
        assert_eq!(i11["burned"], json!(true));
        assert_eq!(i11["starving"], json!(true));
        assert_eq!(i11["ai_court_disagree"], json!(true));
        assert_eq!(i11["ai_low_confidence"], json!(false));
        assert_eq!(i11["judge_est_tokens"], json!(9750));
        assert_eq!(i11["pair_call_id"], Value::Null);

        let i1 = by_id(1);
        assert_eq!(i1["priority"], json!(7));
        assert_eq!(i1["ai_court_disagree"], json!(false)); // superseded tag ignored
        assert_eq!(i1["judge_est_tokens"], json!(6150));

        let i2 = by_id(2);
        assert_eq!(i2["priority"], json!(6));
        assert_eq!(i2["burned"], json!(false));
        assert_eq!(i2["starving"], json!(false));
        assert_eq!(i2["ai_low_confidence"], json!(true));
        assert_eq!(i2["judge_est_tokens"], json!(1550));

        assert_eq!(by_id(3)["priority"], json!(3));
        assert_eq!(by_id(4)["priority"], json!(3));
        assert_eq!(by_id(3)["pair_call_id"], json!(4));
        assert_eq!(by_id(4)["pair_call_id"], json!(3));

        assert_eq!(r["category_progress"], json!([
            { "category": "conversational", "have": 25, "need": 12, "speaking": true },
            { "category": "writing", "have": 5, "need": 12, "speaking": false },
            { "category": "research", "have": 0, "need": 12, "speaking": false },
        ]));
        assert_eq!(r["progress"], json!({ "tagged": 1, "skipped": 1, "remaining": 5 }));

        let run_cards: Vec<String> = r["runs"].as_array().unwrap().iter().map(|x| x["card_slug"].as_str().unwrap().to_string()).collect();
        assert_eq!(run_cards, vec!["CARD-C", "CARD-A", "CARD-B"]);
        let run_by = |slug: &str| r["runs"].as_array().unwrap().iter().find(|x| x["card_slug"] == json!(slug)).unwrap();
        assert_eq!(run_by("CARD-A")["calls"], json!(7));
        assert_eq!(run_by("CARD-A")["untagged"], json!(5));
        assert_eq!(run_by("CARD-B")["calls"], json!(2));
        assert_eq!(run_by("CARD-C")["calls"], json!(1));
    }

    #[test]
    fn filters_and_limit_and_empty_db() {
        let db = seeded_db();
        assert_eq!(ids(&heimdallTagQueue(&json!({ "category": "writing" }), &db).unwrap()), vec![2]);
        assert_eq!(ids(&heimdallTagQueue(&json!({ "template_base": "chat" }), &db).unwrap()), vec![11, 1, 4, 3]);
        assert_eq!(ids(&heimdallTagQueue(&json!({ "card_slug": "CARD-A" }), &db).unwrap()), vec![1, 2]); // chronological
        let card_b = heimdallTagQueue(&json!({ "card_slug": "CARD-B" }), &db).unwrap();
        assert_eq!(ids(&card_b), vec![3, 4]);
        assert_eq!(card_b["items"][0]["pair_call_id"], json!(4));
        assert_eq!(card_b["items"][1]["pair_call_id"], json!(3));
        assert_eq!(ids(&heimdallTagQueue(&json!({ "limit": 2 }), &db).unwrap()), vec![11, 1]);
        assert_eq!(ids(&heimdallTagQueue(&json!({ "limit": 0 }), &db).unwrap()), vec![11]); // clamped up to 1

        let empty_db = Connection::open_in_memory().unwrap();
        schema(&empty_db);
        let r = heimdallTagQueue(&json!({}), &empty_db).unwrap();
        assert_eq!(r, json!({ "ok": true, "items": [], "progress": { "tagged": 0, "skipped": 0, "remaining": 0 }, "category_progress": [], "runs": [] }));
    }
}
