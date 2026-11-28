//! audit-timeline.rs — twin of skills/system/audit-timeline.mjs (db READ, attach lane).
//! NAMES ARE CONTRACT: `auditTimeline` (default). Paginated sacred_timeline reader with cognition
//! data LEFT JOINed from brain.ai_calls (+ brain.prompt_templates for the template name), plus the
//! Phase-4 live unsealed staging trail from timeline.audit_stage. Read-only, pure SQL aggregation.
// QUALITY: 78/100 — dynamic WHERE assembly + size-probed/budgeted blob fetch + diff_summary
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `!= null`
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// node:sqlite binds integer-valued JS numbers as INTEGER; NaN/Infinity land as NULL (json! → Null)
fn num_param(v: f64) -> Value { if v.is_finite() && v.fract() == 0.0 { json!(v as i64) } else { json!(v) } }

const PER_ROW_BLOB_CAP: i64 = 2 * 1024 * 1024;
const TOTAL_BLOB_BUDGET: i64 = 8 * 1024 * 1024;

pub fn auditTimeline(input: &Value, db: &Connection) -> Result<Value, String> {
    // cappedLimit = Math.min(Math.max(1, Number(limit) || 100), 500)
    let n = match input.get("limit") { None => 100.0, v => js_number(v) };
    let n = if n == 0.0 || n.is_nan() { 100.0 } else { n }; // JS `Number(limit) || 100`
    let capped_limit = n.max(1.0).min(500.0);

    let ai_only = truthy(input.get("ai_only"));

    let mut where_parts: Vec<&str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(sid) = nz(input.get("since_id")) {
        where_parts.push("s.id < @since_id");
        params.push(num_param(js_number(Some(sid))));
    }
    if truthy(input.get("entity_slug")) {
        where_parts.push("s.entity_id = @entity_slug");
        params.push(json!(js_string(input.get("entity_slug").unwrap())));
    }
    if truthy(input.get("entity_type")) {
        where_parts.push("s.entity_type = @entity_type");
        params.push(json!(js_string(input.get("entity_type").unwrap())));
    }
    if truthy(input.get("actor")) {
        where_parts.push("s.actor = @actor");
        params.push(json!(js_string(input.get("actor").unwrap())));
    }
    if truthy(input.get("operation")) {
        where_parts.push("s.operation = @operation");
        params.push(json!(js_string(input.get("operation").unwrap())));
    }
    if let Some(uid) = nz(input.get("user_goal_id")) {
        where_parts.push("s.user_goal_id = @user_goal_id");
        params.push(num_param(js_number(Some(uid))));
    }
    if ai_only {
        where_parts.push("ac.id IS NOT NULL");
    }
    let where_clause = if where_parts.is_empty() { String::new() } else { format!("WHERE {}", where_parts.join(" AND ")) };

    let rows_sql = format!(
        "\n    SELECT\n      s.id, s.ts, s.actor, s.operation, s.entity_type, s.entity_id,\n      s.user_goal_id,\n      ac.id AS ai_call_id, ac.tier, ac.model_id, ac.cost_usd_actual,\n      ac.duration_ms, pt.name AS prompt_template\n    FROM sacred_timeline s\n    LEFT JOIN brain.ai_calls ac ON ac.parent_event_id = s.id\n    LEFT JOIN brain.prompt_templates pt ON pt.id = ac.prompt_template_id\n    {}\n    ORDER BY s.id DESC\n    LIMIT @limit\n  ",
        where_clause
    );
    // named placeholders bind positionally in first-appearance order — same order as pushed above
    let mut rows_params = params.clone();
    rows_params.push(num_param(capped_limit));
    let rows = crate::db_json::query_rows(db, &rows_sql, &rows_params)?;
    let rows_arr: Vec<Value> = rows.as_array().cloned().unwrap_or_default();

    // Size probe (header-only octet_length, no blob read) → budgeted fetch.
    let mut blob_by_id: HashMap<i64, Value> = HashMap::new();
    if !rows_arr.is_empty() {
        let ids: Vec<i64> = rows_arr.iter().map(|r| r["id"].as_i64().unwrap()).collect();
        let ph = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sizes_sql = format!(
            "SELECT id,\n             COALESCE(octet_length(before_json), 0) + COALESCE(octet_length(after_json), 0) AS bytes\n      FROM sacred_timeline WHERE id IN ({})",
            ph
        );
        let size_params: Vec<Value> = ids.iter().map(|i| json!(*i)).collect();
        let sizes = crate::db_json::query_rows(db, &sizes_sql, &size_params)?;
        let size_by_id: HashMap<i64, i64> = sizes
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["id"].as_i64().unwrap(), s["bytes"].as_i64().unwrap_or(0)))
            .collect();
        let mut budget = TOTAL_BLOB_BUDGET;
        let mut fetch_ids: Vec<i64> = Vec::new();
        for r in &rows_arr { // rows are newest-first — newest get the budget
            let id = r["id"].as_i64().unwrap();
            let bytes = *size_by_id.get(&id).unwrap_or(&0);
            if bytes <= PER_ROW_BLOB_CAP && bytes <= budget {
                budget -= bytes;
                fetch_ids.push(id);
            }
        }
        if !fetch_ids.is_empty() {
            let ph2 = fetch_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let blobs_sql = format!(
                "SELECT id, before_json, after_json, digital_print\n        FROM sacred_timeline WHERE id IN ({})",
                ph2
            );
            let fetch_params: Vec<Value> = fetch_ids.iter().map(|i| json!(*i)).collect();
            let blobs = crate::db_json::query_rows(db, &blobs_sql, &fetch_params)?;
            for b in blobs.as_array().unwrap() {
                blob_by_id.insert(b["id"].as_i64().unwrap(), b.clone());
            }
        }
    }

    // total count for the filter (without limit). The ai_calls LEFT JOIN can never change the row
    // count — only join when the ai_only filter needs it.
    let total_sql = format!(
        "SELECT COUNT(*) AS c\n    FROM sacred_timeline s\n    {}\n    {}",
        if ai_only { "LEFT JOIN brain.ai_calls ac ON ac.parent_event_id = s.id" } else { "" },
        where_clause
    );
    let total_rows = crate::db_json::query_rows(db, &total_sql, &params)?;
    let total = total_rows
        .as_array()
        .and_then(|a| a.first())
        .and_then(|r| r.get("c"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let mut entries: Vec<Value> = Vec::new();
    for r in &rows_arr {
        let id = r["id"].as_i64().unwrap();
        let blobs = blob_by_id.get(&id);

        // Compute a compact diff_summary from before/after JSON — top-level keys that changed.
        // null when the row's blobs were over the fetch budget (still fully present in the chain).
        let mut diff_summary = Value::Null;
        if let Some(b) = blobs {
            let before_val = b.get("before_json");
            let after_val = b.get("after_json");
            // JS: `blobs.before_json ? JSON.parse(blobs.before_json) : {}` — a TRUTHY check (not
            // just non-null: an empty-string column would also fall through to {}), same gate as
            // the outer `blobs.before_json || blobs.after_json` entry condition.
            if truthy(before_val) || truthy(after_val) {
                let parse_or_empty = |v: Option<&Value>| -> Result<Value, ()> {
                    if truthy(v) {
                        v.and_then(|x| x.as_str()).ok_or(()).and_then(|s| serde_json::from_str::<Value>(s).map_err(|_| ()))
                    } else {
                        Ok(json!({}))
                    }
                };
                let before_res = parse_or_empty(before_val);
                let after_res = parse_or_empty(after_val);
                if let (Ok(before_obj), Ok(after_obj)) = (before_res, after_res) {
                    let empty: Map<String, Value> = Map::new();
                    let before_map = before_obj.as_object().unwrap_or(&empty);
                    let after_map = after_obj.as_object().unwrap_or(&empty);
                    let mut seen: HashSet<String> = HashSet::new();
                    let mut all_keys: Vec<String> = Vec::new();
                    for k in before_map.keys() { if seen.insert(k.clone()) { all_keys.push(k.clone()); } }
                    for k in after_map.keys() { if seen.insert(k.clone()) { all_keys.push(k.clone()); } }
                    let mut added: Vec<String> = Vec::new();
                    let mut modified: Vec<String> = Vec::new();
                    let mut removed: Vec<String> = Vec::new();
                    for k in &all_keys {
                        if !before_map.contains_key(k) {
                            added.push(k.clone());
                        } else if !after_map.contains_key(k) {
                            removed.push(k.clone());
                        } else {
                            let bv = serde_json::to_string(&before_map[k]).unwrap_or_default();
                            let av = serde_json::to_string(&after_map[k]).unwrap_or_default();
                            if bv != av { modified.push(k.clone()); }
                        }
                    }
                    diff_summary = json!({ "added": added, "modified": modified, "removed": removed });
                } // else: unparseable JSON — skip, diff_summary stays null
            }
        }

        let digital_print = blobs.and_then(|b| b.get("digital_print")).cloned().unwrap_or(Value::Null);

        let mut entry = Map::new();
        entry.insert("id".into(), json!(id));
        entry.insert("ts".into(), r.get("ts").cloned().unwrap_or(Value::Null));
        entry.insert("actor".into(), r.get("actor").cloned().unwrap_or(Value::Null));
        entry.insert("operation".into(), r.get("operation").cloned().unwrap_or(Value::Null));
        entry.insert("entity_type".into(), r.get("entity_type").cloned().unwrap_or(Value::Null));
        entry.insert("entity_id".into(), r.get("entity_id").cloned().unwrap_or(Value::Null));
        entry.insert("digital_print".into(), digital_print);
        entry.insert("diff_summary".into(), diff_summary);
        entry.insert("parent_event_id".into(), Value::Null); // sacred_timeline doesn't model parent chains directly; user_goal_id is the closest field

        let ai_call_id = r.get("ai_call_id").filter(|v| !v.is_null());
        if let Some(aid) = ai_call_id {
            let cost_usd = r.get("cost_usd_actual").filter(|v| !v.is_null()).cloned().unwrap_or(json!(0));
            let mut cognition = Map::new();
            cognition.insert("tier".into(), r.get("tier").cloned().unwrap_or(Value::Null));
            cognition.insert("model_id".into(), r.get("model_id").cloned().unwrap_or(Value::Null));
            cognition.insert("cost_usd".into(), cost_usd);
            cognition.insert("duration_ms".into(), r.get("duration_ms").cloned().unwrap_or(Value::Null));
            cognition.insert("prompt_template".into(), r.get("prompt_template").cloned().unwrap_or(Value::Null));
            cognition.insert("ai_call_id".into(), aid.clone());
            entry.insert("cognition".into(), Value::Object(cognition));
        } // else: cognition stays undefined in JS → key omitted (JSON.stringify drops undefined)

        entries.push(Value::Object(entry));
    }

    let oldest_id_returned = entries.last().and_then(|e| e.get("id")).cloned().unwrap_or(Value::Null);
    let has_more = (entries.len() as f64) == capped_limit;

    // EVENT-SOURCED-AUDIT (Phase 4): the LIVE unsealed trail — rows staged for the in-flight run(s)
    // the driver hasn't yet sealed into sacred_timeline. Light columns only (no blobs); same
    // entity/actor filters (NOT operation/user_goal_id/since_id/ai_only). Empty on a pre-migration
    // DB or when staging is off (mirrors JS try/catch around this block).
    let mut s_where: Vec<&str> = Vec::new();
    let mut s_params: Vec<Value> = Vec::new();
    if truthy(input.get("entity_slug")) {
        s_where.push("entity_id = @entity_slug");
        s_params.push(json!(js_string(input.get("entity_slug").unwrap())));
    }
    if truthy(input.get("entity_type")) {
        s_where.push("entity_type = @entity_type");
        s_params.push(json!(js_string(input.get("entity_type").unwrap())));
    }
    if truthy(input.get("actor")) {
        s_where.push("actor = @actor");
        s_params.push(json!(js_string(input.get("actor").unwrap())));
    }
    let s_clause = if s_where.is_empty() { String::new() } else { format!("WHERE {}", s_where.join(" AND ")) };
    let staged_sql = format!(
        "\n      SELECT id, ts, actor, operation, entity_type, entity_id, session_id, phase, tick_id\n      FROM timeline.audit_stage\n      {}\n      ORDER BY id ASC\n      LIMIT @limit\n    ",
        s_clause
    );
    s_params.push(num_param(capped_limit));
    let staged = crate::db_json::query_rows(db, &staged_sql, &s_params)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();

    let mut out = Map::new();
    out.insert("entries".into(), json!(entries));
    out.insert("total".into(), json!(total));
    out.insert("oldest_id_returned".into(), oldest_id_returned);
    out.insert("has_more".into(), json!(has_more));
    out.insert("staged".into(), json!(staged));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; ATTACH ':memory:' AS timeline; \
             CREATE TABLE sacred_timeline ( \
               id INTEGER PRIMARY KEY, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT, \
               user_goal_id INTEGER, before_json TEXT, after_json TEXT, digital_print TEXT \
             ); \
             CREATE TABLE brain.ai_calls ( \
               id INTEGER PRIMARY KEY, parent_event_id INTEGER, tier TEXT, model_id TEXT, \
               cost_usd_actual REAL, duration_ms REAL, prompt_template_id INTEGER \
             ); \
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT ); \
             CREATE TABLE timeline.audit_stage ( \
               id INTEGER PRIMARY KEY, session_id TEXT, tick_id INTEGER, phase TEXT, ts TEXT, actor TEXT, \
               operation TEXT, entity_type TEXT, entity_id TEXT \
             ); \
             INSERT INTO sacred_timeline (id, ts, actor, operation, entity_type, entity_id, user_goal_id, before_json, after_json, digital_print) VALUES \
               (1, '2025-12-01 00:00:00', 'dev', 'card.add', 'card', 'CARD-A', NULL, NULL, '{\"status\":\"backlog\"}', 'dp1'), \
               (2, '2025-12-05 00:00:00', 'dev', 'card.status.change', 'card', 'CARD-A', 7, '{\"status\":\"backlog\",\"title\":\"Old\"}', '{\"status\":\"doing\",\"title\":\"Old\"}', 'dp2'), \
               (3, '2025-12-10 00:00:00', 'man', 'card.update', 'card', 'CARD-B', NULL, '{\"title\":\"B1\",\"flag\":true}', '{\"title\":\"B2\",\"owner\":\"man\"}', 'dp3'), \
               (4, '2025-12-15 00:00:00', 'system:reconciler', 'memory.add', 'memory', 'MEM-1', 0, NULL, NULL, 'dp4'), \
               (5, '2025-12-20 00:00:00', 'driver:pl', 'card.status.change', 'card', 'CARD-A', NULL, '{\"status\":\"doing\"}', '{\"status\":\"done\"}', 'dp5'); \
             INSERT INTO brain.prompt_templates (id, name) VALUES (10, 'card-work-plan'); \
             INSERT INTO brain.ai_calls (id, parent_event_id, tier, model_id, cost_usd_actual, duration_ms, prompt_template_id) VALUES \
               (100, 2, 'mid', 'claude-sonnet', 0.02, 1500.0, 10), \
               (101, 5, 'high', NULL, NULL, NULL, NULL); \
             INSERT INTO timeline.audit_stage (id, session_id, tick_id, phase, ts, actor, operation, entity_type, entity_id) VALUES \
               (1, 'S1', 5, 'plan', '2025-12-25 00:00:00', 'dev', 'card.add', 'card', 'CARD-C'), \
               (2, 'S1', 5, 'plan', '2025-12-25 00:00:01', 'dev', 'card.update', 'card', 'CARD-C'), \
               (3, 'S2', 6, 'act', '2025-12-26 00:00:00', 'man', 'memory.add', 'memory', 'MEM-9');",
        )
        .unwrap();
        c
    }
    fn ids(r: &Value) -> Vec<i64> { r["entries"].as_array().unwrap().iter().map(|e| e["id"].as_i64().unwrap()).collect() }

    #[test]
    fn defaults_newest_first_with_cognition_and_diff() {
        let c = db();
        let r = auditTimeline(&json!({}), &c).unwrap();
        assert_eq!(ids(&r), vec![5, 4, 3, 2, 1]);
        assert_eq!(r["total"], json!(5));
        assert_eq!(r["oldest_id_returned"], json!(1));
        assert_eq!(r["has_more"], json!(false));

        let e5 = &r["entries"][0];
        assert_eq!(e5["cognition"], json!({ "tier": "high", "model_id": null, "cost_usd": 0, "duration_ms": null, "prompt_template": null, "ai_call_id": 101 }));
        assert_eq!(e5["diff_summary"], json!({ "added": [], "modified": ["status"], "removed": [] }));

        let e4 = &r["entries"][1];
        assert!(e4.get("cognition").is_none(), "no ai_call join → cognition key omitted");
        assert_eq!(e4["diff_summary"], Value::Null, "before/after both null → no diff computed");

        let e3 = &r["entries"][2];
        assert_eq!(e3["diff_summary"], json!({ "added": ["owner"], "modified": ["title"], "removed": ["flag"] }));

        let e2 = &r["entries"][3];
        assert_eq!(e2["cognition"]["ai_call_id"], json!(100));
        assert_eq!(e2["cognition"]["prompt_template"], json!("card-work-plan"));

        let e1 = &r["entries"][4];
        assert_eq!(e1["diff_summary"], json!({ "added": ["status"], "modified": [], "removed": [] }));

        let staged = r["staged"].as_array().unwrap();
        assert_eq!(staged.len(), 3);
        assert_eq!(staged[0]["id"], json!(1));
        assert_eq!(staged[2]["actor"], json!("man"));
    }

    #[test]
    fn since_id_filters_and_number_coercion() {
        let c = db();
        assert_eq!(ids(&auditTimeline(&json!({ "since_id": 3 }), &c).unwrap()), vec![2, 1]);
        assert_eq!(ids(&auditTimeline(&json!({ "since_id": "3" }), &c).unwrap()), vec![2, 1]);
        // since_id: 0 is `!= null` → filter applies (unlike a truthy check)
        let r0 = auditTimeline(&json!({ "since_id": 0 }), &c).unwrap();
        assert_eq!(ids(&r0), Vec::<i64>::new());
        assert_eq!(r0["total"], json!(0));
        // since_id doesn't filter the staged trail
        assert_eq!(r0["staged"].as_array().unwrap().len(), 3);
        // since_id: null → `!= null` skips the filter
        assert_eq!(ids(&auditTimeline(&json!({ "since_id": null }), &c).unwrap()), vec![5, 4, 3, 2, 1]);
    }

    #[test]
    fn entity_slug_entity_type_actor_operation_filters() {
        let c = db();
        assert_eq!(ids(&auditTimeline(&json!({ "entity_slug": "CARD-A" }), &c).unwrap()), vec![5, 2, 1]);
        let r_mem = auditTimeline(&json!({ "entity_type": "memory" }), &c).unwrap();
        assert_eq!(ids(&r_mem), vec![4]);
        assert_eq!(r_mem["staged"].as_array().unwrap().len(), 1, "entity_type filters staged too");
        let r_actor = auditTimeline(&json!({ "actor": "dev" }), &c).unwrap();
        assert_eq!(ids(&r_actor), vec![2, 1]);
        assert_eq!(r_actor["staged"].as_array().unwrap().len(), 2, "actor filters staged too");
        assert_eq!(ids(&auditTimeline(&json!({ "operation": "card.status.change" }), &c).unwrap()), vec![5, 2]);
    }

    #[test]
    fn user_goal_id_allows_zero_ai_only_and_limit_clamp() {
        let c = db();
        // user_goal_id: 0 uses `!= null` (not truthy) — 0 still filters
        assert_eq!(ids(&auditTimeline(&json!({ "user_goal_id": 0 }), &c).unwrap()), vec![4]);
        let r_ai = auditTimeline(&json!({ "ai_only": true }), &c).unwrap();
        assert_eq!(ids(&r_ai), vec![5, 2]);
        assert_eq!(r_ai["total"], json!(2));

        let r_lim = auditTimeline(&json!({ "limit": 2 }), &c).unwrap();
        assert_eq!(ids(&r_lim), vec![5, 4]);
        assert_eq!(r_lim["has_more"], json!(true));
        assert_eq!(r_lim["oldest_id_returned"], json!(4));
        assert_eq!(r_lim["staged"].as_array().unwrap().len(), 2, "limit clamps the staged query too");

        assert_eq!(ids(&auditTimeline(&json!({ "limit": 0 }), &c).unwrap()), vec![5, 4, 3, 2, 1]); // 0 || 100 → default
        let r_neg = auditTimeline(&json!({ "limit": -5 }), &c).unwrap();
        assert_eq!(ids(&r_neg), vec![5]); // Math.max(1, -5) → 1
        assert_eq!(r_neg["has_more"], json!(true));
    }

    #[test]
    fn no_match_returns_empty_shapes() {
        let c = db();
        let r = auditTimeline(&json!({ "entity_slug": "NONEXISTENT" }), &c).unwrap();
        assert_eq!(r["entries"], json!([]));
        assert_eq!(r["total"], json!(0));
        assert_eq!(r["oldest_id_returned"], Value::Null);
        assert_eq!(r["has_more"], json!(false));
        assert_eq!(r["staged"], json!([]));
    }

    #[test]
    fn empty_tables() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; ATTACH ':memory:' AS timeline; \
             CREATE TABLE sacred_timeline ( id INTEGER PRIMARY KEY, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT, user_goal_id INTEGER, before_json TEXT, after_json TEXT, digital_print TEXT ); \
             CREATE TABLE brain.ai_calls ( id INTEGER PRIMARY KEY, parent_event_id INTEGER, tier TEXT, model_id TEXT, cost_usd_actual REAL, duration_ms REAL, prompt_template_id INTEGER ); \
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT ); \
             CREATE TABLE timeline.audit_stage ( id INTEGER PRIMARY KEY, session_id TEXT, tick_id INTEGER, phase TEXT, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT );",
        )
        .unwrap();
        let r = auditTimeline(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "entries": [], "total": 0, "oldest_id_returned": null, "has_more": false, "staged": [] }));
    }
}
