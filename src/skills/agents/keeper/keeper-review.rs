//! keeper-review.rs — twin of skills/agents/keeper/keeper-review.mjs (db READ lane; also WRITES a
//! brain.brain_decisions row via the already-converted log-decision twin). NAMES ARE CONTRACT:
//! `keeperReview` (default). Periodic memory-hygiene scan, read-only report (no card mutation):
//!   - unvalidated_recent: lessons/corrections/decisions/preferences added within the window,
//!     not yet validated, and used at least once.
//!   - consolidation_candidates: greedy tag-overlap clusters (>=2 shared tags) of >=min_cluster_size
//!     tier-1 memories not yet linked into a narrative (no inbound derived_from edge).
//!   - orphan_memories: memories older than the window with no links and no uses.
//! Card: FEAT-AGENT-KEEPER.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

const DEFAULT_WINDOW_DAYS: f64 = 7.0;
const DEFAULT_MIN_CLUSTER: f64 = 3.0;

// = JS Number(v)
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
// JS `${n}` for a finite number: integers render without a decimal point.
fn js_num_str(f: f64) -> String {
    if f == 0.0 { return "0".into(); }
    if f == f.trunc() && f.abs() < 1e15 { return format!("{}", f as i64); }
    format!("{}", f)
}
// Same value, as a serde_json Value: integer-valued -> Number::Integer (JSON.stringify prints "7",
// not "7.0"), genuinely fractional -> f64 passed through unchanged.
fn js_num_value(f: f64) -> Value {
    if f.is_finite() && f == f.trunc() && f.abs() < 1e15 { json!(f as i64) } else { json!(f) }
}
// = JS String(v) (no ??) — used only for non-string elements inside a tags_json array.
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

// JS: `new Set(JSON.parse(m.tags_json || '[]'))` wrapped in try/catch -> empty Set on ANY failure.
// tags_json here is already `COALESCE(m.tags_json, '[]')` from SQL (never SQL-NULL), so the `||`
// fallback only fires on an empty-string cell. JSON.parse can succeed on non-array JSON (an object,
// a number, a bare string); `new Set(x)` then throws for any non-iterable x, which the same catch
// swallows to an empty Set — a parse-successful non-array collapses to empty here too. (The one
// divergence from true JS: `new Set("ab")` would iterate the string's characters; tags_json is
// always array-shaped in practice, so that edge is not modeled.) Insertion order is preserved
// (dedup-by-first-occurrence) to mirror Set's iteration order, since `[...set]` order feeds
// `shared_tags` output.
fn tags_of(tags_json: Option<&Value>) -> Vec<String> {
    let raw = match tags_json { Some(Value::String(s)) if !s.is_empty() => s.as_str(), _ => "[]" };
    let parsed: Value = match serde_json::from_str(raw) { Ok(v) => v, Err(_) => return Vec::new() };
    let arr = match parsed.as_array() { Some(a) => a, None => return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    for v in arr {
        let t = match v { Value::String(s) => s.clone(), other => js_string(other) };
        if !out.contains(&t) { out.push(t); }
    }
    out
}

struct Cluster { shared_tags: Vec<String>, members: Vec<(Value, Value)> } // members: (id, title)

pub fn keeperReview(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('keeper-review','db') when no db — the db lane always injects
    // one, so that guard is unreachable here.
    let wn = js_number(input.get("window_days"));
    let window_days = if wn.is_finite() { wn } else { DEFAULT_WINDOW_DAYS };
    let mn = js_number(input.get("min_cluster_size"));
    let min_cluster_size = if mn.is_finite() { mn } else { DEFAULT_MIN_CLUSTER };
    let modifier = format!("-{} days", js_num_str(window_days));

    // ── 1. Unvalidated memories from the recent window that have been used ──
    let unvalidated_recent = crate::db_json::query_rows(
        db,
        &format!("SELECT m.id, m.type, m.title, m.confidence, m.created_at, \
           (SELECT COUNT(*) FROM memory.memory_uses u WHERE u.memory_id = m.id) AS uses \
         FROM memory.memories m \
         WHERE m.type IN ('lesson','correction','decision','preference') \
           AND COALESCE(m.validated, 0) = 0 \
           AND COALESCE(m.superseded_by, '') = '' \
           AND m.created_at >= datetime({}, ?) \
           AND EXISTS (SELECT 1 FROM memory.memory_uses u WHERE u.memory_id = m.id) \
         ORDER BY uses DESC, m.created_at DESC \
         LIMIT 50", crate::clock::now_sql_inner()),
        &[json!(modifier)],
    )?;
    let unvalidated_recent = unvalidated_recent.as_array().cloned().unwrap_or_default();

    // ── 2. Consolidation candidates — tag-overlap clusters not yet narrated ──
    let tier1 = crate::db_json::query_rows(
        db,
        "SELECT m.id, m.title, COALESCE(m.tags_json, '[]') AS tags_json \
         FROM memory.memories m \
         WHERE m.type IN ('lesson','correction','decision') \
           AND COALESCE(m.superseded_by, '') = '' \
           AND NOT EXISTS ( \
             SELECT 1 FROM memory.memory_links l \
             WHERE l.to_memory_id = m.id AND l.kind = 'derived_from' \
           )",
        &[],
    )?;
    let tier1 = tier1.as_array().cloned().unwrap_or_default();
    let enriched: Vec<(Value, Value, Vec<String>)> = tier1
        .iter()
        .map(|m| (m.get("id").cloned().unwrap_or(Value::Null), m.get("title").cloned().unwrap_or(Value::Null), tags_of(m.get("tags_json"))))
        .collect();

    // Greedy clustering: walk each memory, attach to first existing cluster sharing >=2 tags;
    // else start a new cluster. Order matters — mirrors the JS for-loop exactly.
    let mut clusters: Vec<Cluster> = Vec::new();
    for (id, title, tagset) in &enriched {
        if tagset.len() < 2 { continue; }
        let mut placed = false;
        for c in clusters.iter_mut() {
            let overlap: Vec<String> = tagset.iter().filter(|t| c.shared_tags.contains(t)).cloned().collect();
            if overlap.len() >= 2 {
                c.members.push((id.clone(), title.clone()));
                c.shared_tags = overlap; // shrink to running intersection
                placed = true;
                break;
            }
        }
        if !placed {
            clusters.push(Cluster { shared_tags: tagset.clone(), members: vec![(id.clone(), title.clone())] });
        }
    }
    let consolidation_candidates: Vec<Value> = clusters
        .iter()
        .filter(|c| (c.members.len() as f64) >= min_cluster_size)
        .map(|c| {
            let member_ids: Vec<Value> = c.members.iter().map(|(id, _)| id.clone()).collect();
            let member_titles: Vec<Value> = c.members.iter().map(|(_, t)| t.clone()).collect();
            let shared_tags: Vec<Value> = c.shared_tags.iter().map(|t| json!(t)).collect();
            json!({
                "member_ids": member_ids,
                "member_titles": member_titles,
                "shared_tags": shared_tags,
                "suggested_action": "keeper.memory.consolidate",
            })
        })
        .collect();

    // ── 3. Orphan memories — no links, no uses, older than window ───────────
    let orphan_memories = crate::db_json::query_rows(
        db,
        &format!("SELECT m.id, m.type, m.title, m.created_at \
         FROM memory.memories m \
         WHERE COALESCE(m.superseded_by, '') = '' \
           AND m.created_at < datetime({}, ?) \
           AND NOT EXISTS ( \
             SELECT 1 FROM memory.memory_links l \
             WHERE l.from_memory_id = m.id OR l.to_memory_id = m.id \
           ) \
           AND NOT EXISTS ( \
             SELECT 1 FROM memory.memory_uses u WHERE u.memory_id = m.id \
           ) \
         ORDER BY m.created_at ASC \
         LIMIT 50", crate::clock::now_sql_inner()),
        &[json!(modifier)],
    )?;
    let orphan_memories = orphan_memories.as_array().cloned().unwrap_or_default();

    // ── 4. Summary line for the audit log ───────────────────────────────────
    let summary_md = format!(
        "keeper.review: {} unvalidated-used, {} clusters, {} orphans (window={}d, min_cluster={})",
        unvalidated_recent.len(),
        consolidation_candidates.len(),
        orphan_memories.len(),
        js_num_str(window_days),
        js_num_str(min_cluster_size),
    );

    // FEAT-AGENT-REASONING-LOG: capture each review pass. logDecision swallows its own failures
    // (never throws), matching the JS call site which is not wrapped in try/catch either.
    let log_input = json!({
        "agent": "keeper",
        "decision": "review_pass",
        "rationale": summary_md,
        "inputs": {
            "window_days": js_num_value(window_days),
            "min_cluster_size": js_num_value(min_cluster_size),
            "unvalidated_count": unvalidated_recent.len() as i64,
            "cluster_count": consolidation_candidates.len() as i64,
            "orphan_count": orphan_memories.len() as i64,
        },
    });
    crate::engine_log_decision::logDecision(&log_input, db)?;

    let mut out = Map::new();
    out.insert("scanned_at".into(), json!(crate::clock::now_iso_nodb())); // frozen-clock lane: new Date().toISOString()
    out.insert("window_days".into(), js_num_value(window_days));
    out.insert("min_cluster_size".into(), js_num_value(min_cluster_size));
    out.insert("unvalidated_recent".into(), Value::Array(unvalidated_recent));
    out.insert("consolidation_candidates".into(), Value::Array(consolidation_candidates));
    out.insert("orphan_memories".into(), Value::Array(orphan_memories));
    out.insert("summary_md".into(), json!(summary_md));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory; ATTACH ':memory:' AS brain; \
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, type TEXT, title TEXT, confidence REAL, validated INTEGER, superseded_by INTEGER, tags_json TEXT, created_at TEXT); \
             CREATE TABLE memory.memory_uses (id INTEGER PRIMARY KEY AUTOINCREMENT, memory_id INTEGER); \
             CREATE TABLE memory.memory_links (id INTEGER PRIMARY KEY AUTOINCREMENT, from_memory_id INTEGER, to_memory_id INTEGER, kind TEXT); \
             CREATE TABLE brain.brain_decisions ( id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, decided_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), card_slug TEXT, decision TEXT NOT NULL, rationale TEXT NOT NULL, inputs_json TEXT, fire_id INTEGER, CHECK (decision IN ('fire','wait','rollback','escalate','done','pause','session_start','session_end','goal_set','classify','gate_verdict','rule_match','sandbox_create','review_pass','schedule_and_dispatch','agent_step')) );",
        )
        .unwrap();
        c
    }

    #[test]
    fn empty_memory_all_sections_empty_and_logs_a_review_pass() {
        let c = db();
        let r = keeperReview(&json!({}), &c).unwrap();
        assert_eq!(r["unvalidated_recent"], json!([]));
        assert_eq!(r["consolidation_candidates"], json!([]));
        assert_eq!(r["orphan_memories"], json!([]));
        assert_eq!(r["window_days"], json!(7));
        assert_eq!(r["min_cluster_size"], json!(3));
        assert_eq!(r["scanned_at"], json!("2026-01-01T00:00:00.000Z"));
        assert_eq!(r["summary_md"], json!("keeper.review: 0 unvalidated-used, 0 clusters, 0 orphans (window=7d, min_cluster=3)"));
        let logged = crate::db_json::query_rows(&c, "SELECT decision, rationale FROM brain.brain_decisions", &[]).unwrap();
        assert_eq!(logged, json!([{ "decision": "review_pass", "rationale": r["summary_md"] }]));
    }

    #[test]
    fn surfaces_unvalidated_memories_that_have_been_used() {
        let c = db();
        c.execute_batch(
            "INSERT INTO memory.memories (id,type,title,confidence,validated,superseded_by,tags_json,created_at) VALUES \
               (1,'lesson','used-recent',0.6,0,NULL,NULL,'2025-12-28 00:00:00'), \
               (2,'lesson','not-used',0.6,0,NULL,NULL,'2025-12-28 00:00:00'), \
               (3,'lesson','validated',0.6,1,NULL,NULL,'2025-12-28 00:00:00'), \
               (4,'lesson','superseded',0.6,0,99,NULL,'2025-12-28 00:00:00'), \
               (5,'lesson','too-old',0.6,0,NULL,NULL,'2025-12-01 00:00:00'), \
               (6,'preference','preference-used',0.6,0,NULL,NULL,'2025-12-28 00:00:00'), \
               (7,'reference','wrong-type',0.6,0,NULL,NULL,'2025-12-28 00:00:00'); \
             INSERT INTO memory.memory_uses (memory_id) VALUES (1),(3),(4),(5),(6),(6),(7);",
        )
        .unwrap();
        let r = keeperReview(&json!({}), &c).unwrap();
        let rows = r["unvalidated_recent"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        // uses DESC: id=6 has 2 uses, id=1 has 1 use.
        assert_eq!(rows[0]["id"], json!(6));
        assert_eq!(rows[0]["uses"], json!(2));
        assert_eq!(rows[1]["id"], json!(1));
        assert_eq!(rows[1]["uses"], json!(1));
    }

    #[test]
    fn clusters_memories_sharing_2_tags_and_excludes_narrated_ones() {
        let c = db();
        c.execute_batch(
            "INSERT INTO memory.memories (id,type,title,confidence,validated,superseded_by,tags_json,created_at) VALUES \
               (1,'lesson','cluster-0',0.5,1,NULL,'[\"protocol\",\"contract\",\"unique-0\"]','2025-12-30 00:00:00'), \
               (2,'lesson','cluster-1',0.5,1,NULL,'[\"protocol\",\"contract\",\"unique-1\"]','2025-12-30 00:00:00'), \
               (3,'lesson','cluster-2',0.5,1,NULL,'[\"protocol\",\"contract\",\"unique-2\"]','2025-12-30 00:00:00'), \
               (10,'lesson','a',0.5,1,NULL,'[\"x\",\"y\"]','2025-12-30 00:00:00'), \
               (11,'lesson','b',0.5,1,NULL,'[\"x\",\"y\"]','2025-12-30 00:00:00'), \
               (12,'lesson','c',0.5,1,NULL,'[\"x\",\"y\"]','2025-12-30 00:00:00'), \
               (13,'lesson','N',0.5,1,NULL,NULL,'2025-12-30 00:00:00'), \
               (20,'lesson','solo-a',0.5,1,NULL,'[\"solo-a\",\"solo-b\"]','2025-12-30 00:00:00'), \
               (21,'lesson','solo-b',0.5,1,NULL,'[\"solo-a\",\"solo-b\"]','2025-12-30 00:00:00'), \
               (30,'lesson','single-tag',0.5,1,NULL,'[\"single\"]','2025-12-30 00:00:00'), \
               (40,'lesson','malformed',0.5,1,NULL,'{not valid json','2025-12-30 00:00:00'), \
               (41,'lesson','nonarray',0.5,1,NULL,'{\"a\":1}','2025-12-30 00:00:00'); \
             INSERT INTO memory.memory_links (from_memory_id,to_memory_id,kind) VALUES (13,10,'derived_from'),(13,11,'derived_from'),(13,12,'derived_from');",
        )
        .unwrap();
        let r = keeperReview(&json!({ "min_cluster_size": 3 }), &c).unwrap();
        let cands = r["consolidation_candidates"].as_array().unwrap();
        assert_eq!(cands.len(), 1);
        let ids: Vec<i64> = cands[0]["member_ids"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect();
        assert_eq!(ids, vec![1, 2, 3]);
        assert_eq!(cands[0]["shared_tags"], json!(["protocol", "contract"]));
        assert_eq!(cands[0]["suggested_action"], json!("keeper.memory.consolidate"));
    }

    #[test]
    fn orphan_memories_requires_old_unlinked_and_unused() {
        let c = db();
        c.execute_batch(
            "INSERT INTO memory.memories (id,type,title,confidence,validated,superseded_by,tags_json,created_at) VALUES \
               (1,'lesson','orphan-old',0.5,1,NULL,NULL,'2025-12-01 00:00:00'), \
               (2,'lesson','used-old',0.5,1,NULL,NULL,'2025-12-01 00:00:00'), \
               (3,'lesson','linked-old',0.5,1,NULL,NULL,'2025-12-01 00:00:00'), \
               (4,'lesson','superseded-old',0.5,1,99,NULL,'2025-12-01 00:00:00'), \
               (5,'lesson','recent',0.5,1,NULL,NULL,'2025-12-30 00:00:00'), \
               (6,'correction','orphan-older',0.5,1,NULL,NULL,'2025-11-01 00:00:00'); \
             INSERT INTO memory.memory_uses (memory_id) VALUES (2); \
             INSERT INTO memory.memory_links (from_memory_id,to_memory_id,kind) VALUES (3,500,'related');",
        )
        .unwrap();
        let r = keeperReview(&json!({}), &c).unwrap();
        let ids: Vec<i64> = r["orphan_memories"].as_array().unwrap().iter().map(|v| v["id"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![6, 1]); // ORDER BY created_at ASC
    }

    #[test]
    fn window_and_min_cluster_size_coercion_matches_js_number() {
        let c = db();
        // explicit numeric override
        let r = keeperReview(&json!({ "window_days": 14, "min_cluster_size": 2 }), &c).unwrap();
        assert_eq!(r["window_days"], json!(14));
        assert_eq!(r["min_cluster_size"], json!(2));
        // explicit null -> Number(null) = 0, which IS finite (not the default)
        let r = keeperReview(&json!({ "window_days": null, "min_cluster_size": null }), &c).unwrap();
        assert_eq!(r["window_days"], json!(0));
        assert_eq!(r["min_cluster_size"], json!(0));
        // non-numeric string -> NaN -> falls back to defaults
        let r = keeperReview(&json!({ "window_days": "not-a-number", "min_cluster_size": "also-not" }), &c).unwrap();
        assert_eq!(r["window_days"], json!(7));
        assert_eq!(r["min_cluster_size"], json!(3));
    }
}
