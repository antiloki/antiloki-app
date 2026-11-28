//! heimdall-ai-calibration.rs — twin of skills/heimdall/heimdall-ai-calibration.mjs (db READ lane,
//! cross-db `brain`). NAMES ARE CONTRACT: `heimdallAiCalibration` (default export — `async` in JS,
//! but the body has no `await` at all; the conform JS lane awaits the resolved value regardless, so
//! per playbook §async-default-exports this twin is an ordinary sync fn). The AI judge's report
//! card, three independently try/catch-guarded views over the same brain.heimdall_* tables:
//!   AI-3 calibration      — per hc.category, human-vs-AI verdict-sign agreement rate.
//!   AI-5 court_disagreements — AI judge rated positive but the driver court burned the artifact.
//!   AI-8 weak_gates        — template_base groups with >=50% negative AI verdicts on >=5 tags.
//! Each JS `try { ... } catch { X = defaultValue }` block is mirrored with a per-query
//! `.unwrap_or_else(|_| ...)` instead of `?`-propagating — a query failure (e.g. a table not yet
//! migrated) must fall back to that view's empty default, not fail the whole skill. JS
//! `if (!db) throw err.contextMissing(...)` is unreachable in the db lane (a connection is always
//! injected), so it is not transcribed.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` left-operand filter
fn tmpl(v: Option<&Value>) -> String {
    // JS `${v}` interpolation for the values that appear (string/number/bool/null/absent).
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS Math.round: nearest, exact .5 ties toward +infinity (NOT away from zero, NOT to even).
fn js_round(x: f64) -> f64 { let f = x.floor(); if x - f >= 0.5 { f + 1.0 } else { f } }
// JS `+Number(n).toFixed(d)` for a finite n, collapsed to integer when whole (JSON.stringify(5.0) is "5").
fn to_fixed_num(x: f64, d: usize) -> Value {
    let p: f64 = format!("{:.*}", d, x).parse().unwrap();
    if p.fract() == 0.0 && p.abs() < 9.007_199_254_740_992e15 { json!(p as i64) } else { json!(p) }
}

pub fn heimdallAiCalibration(_input: &Value, db: &Connection) -> Result<Value, String> {
    // ── AI-3: agreement with humans, per category ──
    let calib_rows: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT hc.category,
                COUNT(*) AS both,
                SUM(CASE WHEN (t.verdict > 0) = (h.verdict > 0) THEN 1 ELSE 0 END) AS agree
         FROM brain.heimdall_ai_tags t
         JOIN brain.heimdall_human_tags h ON h.ai_call_id = t.target_ai_call_id
         LEFT JOIN brain.heimdall_castings hc ON hc.ai_call_id = t.target_ai_call_id
         GROUP BY hc.category ORDER BY both DESC",
        &[],
    )
    .ok()
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();

    let calibration: Vec<Value> = calib_rows
        .iter()
        .map(|r| {
            let both = r.get("both").cloned().unwrap_or(Value::Null);
            let agree = r.get("agree").cloned().unwrap_or(Value::Null);
            let both_truthy = both.as_i64().map_or(false, |n| n != 0);
            let pct_val: Value = if both_truthy {
                let a = agree.as_f64().unwrap_or(0.0);
                let b = both.as_f64().unwrap_or(1.0);
                to_fixed_num(a / b, 3)
            } else {
                Value::Null
            };
            let pct_f64 = pct_val.as_f64();
            let both_num = both.as_i64().unwrap_or(0);
            let needs_action = both_num >= 5 && pct_f64.is_some() && pct_f64.unwrap() < 0.7;
            let category_raw = r.get("category").cloned().unwrap_or(Value::Null);
            let category_display = nz(Some(&category_raw)).cloned().unwrap_or_else(|| json!("(uncast)"));
            let recommendation = if needs_action {
                let cat = tmpl(Some(&category_raw));
                json!(format!(
                    "AI judge disagrees with humans {}% in '{}' — commit a lower trust: node bin/antiloki.mjs system.heimdall.trust.set '{{\"provenance\":\"ai_judge\",\"category\":\"{}\",\"trust\":0.2}}'",
                    js_round((1.0 - pct_f64.unwrap()) * 100.0) as i64,
                    cat,
                    cat,
                ))
            } else {
                Value::Null
            };
            json!({
                "category": category_display,
                "both": both,
                "agree": agree,
                "pct": pct_val,
                "recommendation": recommendation,
            })
        })
        .collect();

    // ── AI-5: AI judge vs the driver court ──
    let pairs_all: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT t.target_ai_call_id AS ai_call_id, t.verdict AS ai_verdict, t.confidence, t.reason_md,
                hc.template_base, hc.category, hc.effective_model AS target_model
         FROM brain.heimdall_ai_tags t
         LEFT JOIN brain.heimdall_castings hc ON hc.ai_call_id = t.target_ai_call_id
         WHERE t.verdict > 0
           AND EXISTS (SELECT 1 FROM brain.heimdall_evidence e WHERE e.ai_call_id = t.target_ai_call_id AND e.outcome = 'burn')
         ORDER BY t.confidence DESC",
        &[],
    )
    .ok()
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();
    let court_disagreements = json!({
        "count": pairs_all.len() as i64,
        "pairs": pairs_all.iter().take(20).cloned().collect::<Vec<_>>(),
    });

    // ── AI-8: weak gates (the prompt sensor) ──
    let weak_rows: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT hc.template_base,
                COUNT(*) AS tags,
                SUM(CASE WHEN t.verdict < 0 THEN 1 ELSE 0 END) AS negative,
                (SELECT t2.failure_class FROM brain.heimdall_ai_tags t2
                 LEFT JOIN brain.heimdall_castings h2 ON h2.ai_call_id = t2.target_ai_call_id
                 WHERE h2.template_base = hc.template_base AND t2.failure_class IS NOT NULL
                 GROUP BY t2.failure_class ORDER BY COUNT(*) DESC LIMIT 1) AS top_failure_class
         FROM brain.heimdall_ai_tags t
         JOIN brain.heimdall_castings hc ON hc.ai_call_id = t.target_ai_call_id
         GROUP BY hc.template_base
         HAVING tags >= 5 AND negative * 2 >= tags
         ORDER BY negative * 1.0 / tags DESC",
        &[],
    )
    .ok()
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();

    let weak_gates: Vec<Value> = weak_rows
        .iter()
        .map(|r| {
            let mut obj: Map<String, Value> = r.as_object().cloned().unwrap_or_default();
            let tags = r.get("tags").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let negative = r.get("negative").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let negative_rate = to_fixed_num(negative / tags, 3);
            let tfc = match nz(r.get("top_failure_class")) {
                Some(v) => tmpl(Some(v)),
                None => "?".to_string(),
            };
            let recommendation = format!(
                "\u{2265}50% negative AI verdicts on '{}' (top failure: {}) — the PROMPT likely needs work: run the prompt-quality report on it.",
                tmpl(r.get("template_base")),
                tfc,
            );
            obj.insert("negative_rate".into(), negative_rate);
            obj.insert("recommendation".into(), json!(recommendation));
            Value::Object(obj)
        })
        .collect();

    Ok(json!({
        "ok": true,
        "calibration": calibration,
        "court_disagreements": court_disagreements,
        "weak_gates": weak_gates,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_brain() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain;
             CREATE TABLE brain.heimdall_ai_tags (id INTEGER PRIMARY KEY, target_ai_call_id INTEGER, verdict INTEGER, confidence REAL, reason_md TEXT, failure_class TEXT);
             CREATE TABLE brain.heimdall_human_tags (id INTEGER PRIMARY KEY, ai_call_id INTEGER, verdict INTEGER);
             CREATE TABLE brain.heimdall_castings (id INTEGER PRIMARY KEY, ai_call_id INTEGER, template_base TEXT, category TEXT, effective_model TEXT);
             CREATE TABLE brain.heimdall_evidence (id INTEGER PRIMARY KEY, ai_call_id INTEGER, outcome TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn empty_tables_return_all_defaults() {
        let c = db_with_brain();
        let r = heimdallAiCalibration(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "calibration": [], "court_disagreements": { "count": 0, "pairs": [] }, "weak_gates": [] }));
    }

    #[test]
    fn calibration_recommends_below_threshold_and_uncast_bucket() {
        let c = db_with_brain();
        // category 'billing': 5 shared calls, 1 agreement -> pct 0.2 (<0.7, both>=5) -> recommendation.
        c.execute_batch(
            "INSERT INTO brain.heimdall_castings (ai_call_id, template_base, category, effective_model) VALUES
               (1,'billing.v1','billing','m'), (2,'billing.v1','billing','m'), (3,'billing.v1','billing','m'),
               (4,'billing.v1','billing','m'), (5,'billing.v1','billing','m');
             INSERT INTO brain.heimdall_ai_tags (target_ai_call_id, verdict) VALUES
               (1,1), (2,1), (3,1), (4,1), (5,-1);
             INSERT INTO brain.heimdall_human_tags (ai_call_id, verdict) VALUES
               (1,-1), (2,-1), (3,-1), (4,-1), (5,-1);",
        )
        .unwrap();
        // ai_call_id 6 has no casting row at all -> hc.category is NULL -> '(uncast)' bucket, single pair (both=1 < 5, no recommendation).
        c.execute_batch(
            "INSERT INTO brain.heimdall_ai_tags (target_ai_call_id, verdict) VALUES (6,1);
             INSERT INTO brain.heimdall_human_tags (ai_call_id, verdict) VALUES (6,1);",
        )
        .unwrap();
        let r = heimdallAiCalibration(&json!({}), &c).unwrap();
        let calibration = r["calibration"].as_array().unwrap();
        assert_eq!(calibration.len(), 2);
        let billing = calibration.iter().find(|c| c["category"] == json!("billing")).unwrap();
        assert_eq!(billing["both"], json!(5));
        assert_eq!(billing["agree"], json!(1));
        assert_eq!(billing["pct"], json!(0.2));
        assert_eq!(
            billing["recommendation"],
            json!("AI judge disagrees with humans 80% in 'billing' — commit a lower trust: node bin/antiloki.mjs system.heimdall.trust.set '{\"provenance\":\"ai_judge\",\"category\":\"billing\",\"trust\":0.2}'")
        );
        let uncast = calibration.iter().find(|c| c["category"] == json!("(uncast)")).unwrap();
        assert_eq!(uncast["both"], json!(1));
        assert_eq!(uncast["agree"], json!(1));
        assert_eq!(uncast["pct"], json!(1));
        assert_eq!(uncast["recommendation"], Value::Null);
    }

    #[test]
    fn court_disagreements_filters_positive_burned_and_caps_at_20() {
        let c = db_with_brain();
        // 22 positive-verdict tags each burned by the court -> count=22, pairs capped to 20.
        let mut sql = String::new();
        for i in 1..=22 {
            sql.push_str(&format!(
                "INSERT INTO brain.heimdall_ai_tags (target_ai_call_id, verdict, confidence) VALUES ({i},1,{});\n",
                0.5 + (i as f64) * 0.01
            ));
            sql.push_str(&format!("INSERT INTO brain.heimdall_evidence (ai_call_id, outcome) VALUES ({i},'burn');\n"));
        }
        // one negative-verdict + burned row must be excluded (t.verdict > 0 filter).
        sql.push_str("INSERT INTO brain.heimdall_ai_tags (target_ai_call_id, verdict) VALUES (100,-1);\n");
        sql.push_str("INSERT INTO brain.heimdall_evidence (ai_call_id, outcome) VALUES (100,'burn');\n");
        // one positive-verdict, non-burned row must be excluded (EXISTS filter).
        sql.push_str("INSERT INTO brain.heimdall_ai_tags (target_ai_call_id, verdict) VALUES (200,1);\n");
        sql.push_str("INSERT INTO brain.heimdall_evidence (ai_call_id, outcome) VALUES (200,'clean');\n");
        c.execute_batch(&sql).unwrap();
        let r = heimdallAiCalibration(&json!({}), &c).unwrap();
        assert_eq!(r["court_disagreements"]["count"], json!(22));
        let pairs = r["court_disagreements"]["pairs"].as_array().unwrap();
        assert_eq!(pairs.len(), 20);
        // ORDER BY t.confidence DESC — call 22 has the highest confidence (0.5+0.22).
        assert_eq!(pairs[0]["ai_call_id"], json!(22));
    }

    #[test]
    fn weak_gates_surfaces_gate_with_top_failure_class_and_null_fallback() {
        let c = db_with_brain();
        // template 'chat.v3': 5 tags, 3 negative (60% >= 50%) -> surfaced; top failure class 'off-scope' (2 of 3 negatives).
        c.execute_batch(
            "INSERT INTO brain.heimdall_castings (ai_call_id, template_base) VALUES
               (1,'chat.v3'), (2,'chat.v3'), (3,'chat.v3'), (4,'chat.v3'), (5,'chat.v3');
             INSERT INTO brain.heimdall_ai_tags (target_ai_call_id, verdict, failure_class) VALUES
               (1,1,NULL), (2,1,NULL), (3,-1,'off-scope'), (4,-1,'off-scope'), (5,-1,'style');",
        )
        .unwrap();
        // template 'faq.v1': 5 tags, 2 negative (40% < 50%) -> not surfaced (HAVING excludes it).
        c.execute_batch(
            "INSERT INTO brain.heimdall_castings (ai_call_id, template_base) VALUES
               (6,'faq.v1'), (7,'faq.v1'), (8,'faq.v1'), (9,'faq.v1'), (10,'faq.v1');
             INSERT INTO brain.heimdall_ai_tags (target_ai_call_id, verdict, failure_class) VALUES
               (6,1,NULL), (7,1,NULL), (8,1,NULL), (9,-1,NULL), (10,-1,NULL);",
        )
        .unwrap();
        let r = heimdallAiCalibration(&json!({}), &c).unwrap();
        let weak_gates = r["weak_gates"].as_array().unwrap();
        assert_eq!(weak_gates.len(), 1);
        let g = &weak_gates[0];
        assert_eq!(g["template_base"], json!("chat.v3"));
        assert_eq!(g["tags"], json!(5));
        assert_eq!(g["negative"], json!(3));
        assert_eq!(g["negative_rate"], json!(0.6));
        assert_eq!(g["top_failure_class"], json!("off-scope"));
        assert_eq!(
            g["recommendation"],
            json!("\u{2265}50% negative AI verdicts on 'chat.v3' (top failure: off-scope) — the PROMPT likely needs work: run the prompt-quality report on it.")
        );
    }

    #[test]
    fn weak_gates_null_top_failure_class_renders_question_mark() {
        let c = db_with_brain();
        // 5 tags, all 5 negative, none carry a failure_class -> top_failure_class NULL -> '?' fallback.
        c.execute_batch(
            "INSERT INTO brain.heimdall_castings (ai_call_id, template_base) VALUES
               (1,'gate.v1'), (2,'gate.v1'), (3,'gate.v1'), (4,'gate.v1'), (5,'gate.v1');
             INSERT INTO brain.heimdall_ai_tags (target_ai_call_id, verdict, failure_class) VALUES
               (1,-1,NULL), (2,-1,NULL), (3,-1,NULL), (4,-1,NULL), (5,-1,NULL);",
        )
        .unwrap();
        let r = heimdallAiCalibration(&json!({}), &c).unwrap();
        let weak_gates = r["weak_gates"].as_array().unwrap();
        assert_eq!(weak_gates.len(), 1);
        assert_eq!(weak_gates[0]["top_failure_class"], Value::Null);
        assert!(weak_gates[0]["recommendation"].as_str().unwrap().contains("(top failure: ?)"));
    }
}
