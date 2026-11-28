//! cards-active.rs — twin of skills/system/cards-active.mjs (db READ lane). NAMES ARE
//! CONTRACT: `cardsActive` (default). One-trip data source for the pipeline kanban +
//! waterfall view: every doing/awaiting_children/blocked/backlog+play card with derived
//! phase (via system_card_phase::classifyOperation), latest sacred_timeline event, and
//! last recorded contract failure.
// QUALITY: 78/100 — phase-walk fallback to latest event, failByCard join, double-JSON body parse
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

pub fn cardsActive(_input: &Value, db: &Connection) -> Result<Value, String> {
    let cards = crate::db_json::query_rows(db, "
        SELECT id, slug, title, status, body_json,
               risk, complexity, priority, score, driver_intent,
               accuracy_score, accuracy_computed_at,
               awaiting_children_at
        FROM cards
        WHERE status IN ('doing', 'awaiting_children', 'blocked')
           OR (status = 'backlog' AND driver_intent = 'play')
        ORDER BY CASE WHEN status = 'blocked' THEN 0 ELSE 1 END, updated_at DESC
        LIMIT 200
    ", &[])?;
    let cards = cards.as_array().cloned().unwrap_or_default();
    if cards.is_empty() { return Ok(json!([])); }

    let slugs: Vec<Value> = cards.iter().map(|c| c.get("slug").cloned().unwrap_or(Value::Null)).collect();
    let placeholders = slugs.iter().map(|_| "?").collect::<Vec<_>>().join(",");

    // Legacy DBs without the failure columns: swallow the error, leave the map empty
    // (mirrors the JS try/catch).
    let mut fail_by_card: HashMap<String, Value> = HashMap::new();
    if let Ok(rows) = crate::db_json::query_rows(db, &format!(
        "SELECT card_slug, last_failure_kind, last_failure_at
         FROM contracts
         WHERE card_slug IN ({placeholders}) AND superseded_by IS NULL
           AND last_failure_at IS NOT NULL"
    ), &slugs) {
        if let Some(arr) = rows.as_array() {
            for f in arr {
                if let Some(s) = f.get("card_slug").and_then(|v| v.as_str()) {
                    fail_by_card.insert(s.to_string(), f.clone());
                }
            }
        }
    }

    let latest_events = crate::db_json::query_rows(db, &format!(
        "SELECT entity_id, operation, ts, actor, id
         FROM (
           SELECT s.*, ROW_NUMBER() OVER (PARTITION BY entity_id ORDER BY id DESC) AS rn
           FROM sacred_timeline s
           WHERE entity_type='card' AND entity_id IN ({placeholders})
         )
         WHERE rn = 1"
    ), &slugs)?;
    let mut event_by_card: HashMap<String, Value> = HashMap::new();
    if let Some(arr) = latest_events.as_array() {
        for e in arr {
            if let Some(s) = e.get("entity_id").and_then(|v| v.as_str()) {
                event_by_card.insert(s.to_string(), e.clone());
            }
        }
    }

    let mut out: Vec<Value> = Vec::new();
    for c in &cards {
        let slug = c.get("slug").cloned().unwrap_or(Value::Null);
        let slug_str = slug.as_str().unwrap_or("").to_string();

        let recent = crate::db_json::query_rows(db,
            "SELECT operation, ts, actor FROM sacred_timeline WHERE entity_type='card' AND entity_id = ? ORDER BY id DESC LIMIT 50",
            &[slug.clone()])?;

        // Walk newest-first for the most recent CLASSIFIABLE op; the immediate-latest
        // event might be a no-op (status change, note) that doesn't map to a phase.
        let mut current_phase = Value::Null;
        let mut last_classifiable_ts = Value::Null;
        let mut last_classifiable_actor = Value::Null;
        let mut last_classifiable_op = Value::Null;
        if let Some(arr) = recent.as_array() {
            for r in arr {
                let op = r.get("operation").cloned().unwrap_or(Value::Null);
                let p = crate::system_card_phase::classifyOperation(&op);
                if !p.is_null() {
                    current_phase = p;
                    last_classifiable_ts = r.get("ts").cloned().unwrap_or(Value::Null);
                    last_classifiable_actor = r.get("actor").cloned().unwrap_or(Value::Null);
                    last_classifiable_op = r.get("operation").cloned().unwrap_or(Value::Null);
                    break;
                }
            }
        }
        let latest = event_by_card.get(&slug_str);

        // JSON.parse(body_json), swallowing parse failure to null; if the result is
        // ITSELF a JSON string (double-encoded body), parse once more.
        let mut parsed: Value = Value::Null;
        if let Some(s) = c.get("body_json").and_then(|v| v.as_str()) {
            if !s.is_empty() {
                parsed = serde_json::from_str::<Value>(s).unwrap_or(Value::Null);
            }
        }
        if let Value::String(inner) = parsed.clone() {
            if let Ok(v) = serde_json::from_str::<Value>(&inner) { parsed = v; }
        }
        let kind = parsed.get("kind").cloned().unwrap_or(Value::Null);
        let size = parsed.get("size").cloned().unwrap_or(Value::Null);

        let last_failure = fail_by_card.get(&slug_str);

        let mut o = Map::new();
        o.insert("id".into(), c.get("id").cloned().unwrap_or(Value::Null));
        o.insert("slug".into(), slug.clone());
        o.insert("title".into(), c.get("title").cloned().unwrap_or(Value::Null));
        o.insert("status".into(), c.get("status").cloned().unwrap_or(Value::Null));
        o.insert("awaiting_children_at".into(), nz(c.get("awaiting_children_at")).cloned().unwrap_or(Value::Null));
        o.insert("kind".into(), kind);
        o.insert("size".into(), size);
        o.insert("priority".into(), c.get("priority").cloned().unwrap_or(Value::Null));
        o.insert("risk".into(), c.get("risk").cloned().unwrap_or(Value::Null));
        o.insert("complexity".into(), c.get("complexity").cloned().unwrap_or(Value::Null));
        o.insert("score".into(), c.get("score").cloned().unwrap_or(Value::Null));
        o.insert("accuracy_score".into(), nz(c.get("accuracy_score")).cloned().unwrap_or(Value::Null));
        o.insert("accuracy_computed_at".into(), nz(c.get("accuracy_computed_at")).cloned().unwrap_or(Value::Null));
        o.insert("driver_intent".into(), c.get("driver_intent").cloned().unwrap_or(Value::Null));
        o.insert("last_failure_kind".into(), last_failure.and_then(|f| nz(f.get("last_failure_kind"))).cloned().unwrap_or(Value::Null));
        o.insert("last_failure_at".into(), last_failure.and_then(|f| nz(f.get("last_failure_at"))).cloned().unwrap_or(Value::Null));
        o.insert("current_phase".into(), current_phase);
        o.insert("last_event_ts".into(), nz(Some(&last_classifiable_ts)).or_else(|| nz(latest.and_then(|l| l.get("ts")))).cloned().unwrap_or(Value::Null));
        o.insert("last_actor".into(), nz(Some(&last_classifiable_actor)).or_else(|| nz(latest.and_then(|l| l.get("actor")))).cloned().unwrap_or(Value::Null));
        o.insert("last_operation".into(), nz(Some(&last_classifiable_op)).or_else(|| nz(latest.and_then(|l| l.get("operation")))).cloned().unwrap_or(Value::Null));
        o.insert("cost_usd_to_date".into(), json!(0));
        o.insert("tokens_total".into(), json!(0));
        o.insert("ai_calls_count".into(), json!(0));

        out.push(Value::Object(o));
    }

    Ok(json!(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT, title TEXT, status TEXT, body_json TEXT, \
               risk INTEGER, complexity INTEGER, priority INTEGER, score INTEGER, driver_intent TEXT, \
               accuracy_score INTEGER, accuracy_computed_at TEXT, awaiting_children_at TEXT, updated_at TEXT); \
             CREATE TABLE contracts (card_slug TEXT, last_failure_kind TEXT, last_failure_at TEXT, superseded_by TEXT); \
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, entity_type TEXT, entity_id TEXT, operation TEXT, ts TEXT, actor TEXT);",
        ).unwrap();
        c
    }

    #[test]
    fn empty_board_returns_empty_array() {
        let r = cardsActive(&json!({}), &make_db()).unwrap();
        assert_eq!(r, json!([]));
    }

    #[test]
    fn blocked_sorts_first_and_backlog_play_is_in_flight() {
        let c = make_db();
        c.execute_batch(
            "INSERT INTO cards (slug, title, status, driver_intent, updated_at) VALUES \
               ('D-1', 'doing card', 'doing', 'auto', '2025-12-31 00:00:00'), \
               ('B-1', 'blocked card', 'blocked', 'auto', '2025-12-01 00:00:00'), \
               ('X-1', 'backlog card', 'backlog', 'auto', '2025-12-20 00:00:00'), \
               ('T-1', 'thrashing', 'backlog', 'play', '2025-12-10 00:00:00');",
        ).unwrap();
        let r = cardsActive(&json!({}), &c).unwrap();
        let arr = r.as_array().unwrap();
        let slugs: Vec<&str> = arr.iter().map(|x| x["slug"].as_str().unwrap()).collect();
        assert!(slugs.contains(&"B-1"));
        assert!(slugs.contains(&"D-1"));
        assert!(slugs.contains(&"T-1"));
        assert!(!slugs.contains(&"X-1")); // plain backlog stays excluded
        assert_eq!(slugs[0], "B-1"); // blocked sorts first regardless of updated_at
    }

    #[test]
    fn phase_walk_falls_back_to_latest_when_no_classifiable_op() {
        let c = make_db();
        c.execute_batch("INSERT INTO cards (slug, title, status, updated_at) VALUES ('D-1', 'd', 'doing', 't0');").unwrap();
        c.execute_batch(
            "INSERT INTO sacred_timeline (id, entity_type, entity_id, operation, ts, actor) VALUES \
               (1, 'card', 'D-1', 'note.add', 't3', 'sysD');",
        ).unwrap();
        let r = cardsActive(&json!({}), &c).unwrap();
        let card = &r[0];
        assert_eq!(card["current_phase"], Value::Null);
        assert_eq!(card["last_event_ts"], json!("t3"));
        assert_eq!(card["last_actor"], json!("sysD"));
        assert_eq!(card["last_operation"], json!("note.add"));
    }

    #[test]
    fn phase_walk_skips_unclassifiable_latest_to_find_earlier_classifiable() {
        let c = make_db();
        c.execute_batch("INSERT INTO cards (slug, title, status, updated_at) VALUES ('B-1', 'b', 'blocked', 't0');").unwrap();
        c.execute_batch(
            "INSERT INTO sacred_timeline (id, entity_type, entity_id, operation, ts, actor) VALUES \
               (1, 'card', 'B-1', 'card.add', 't1', 'human'), \
               (2, 'card', 'B-1', 'note.add', 't2', 'sys');",
        ).unwrap();
        let r = cardsActive(&json!({}), &c).unwrap();
        let card = &r[0];
        assert_eq!(card["current_phase"], json!("propose"));
        assert_eq!(card["last_event_ts"], json!("t1")); // classifiable event wins over the newer no-op
        assert_eq!(card["last_actor"], json!("human"));
    }

    #[test]
    fn no_events_at_all_yields_all_nulls() {
        let c = make_db();
        c.execute_batch("INSERT INTO cards (slug, title, status, updated_at) VALUES ('A-1', 'a', 'awaiting_children', 't0');").unwrap();
        let r = cardsActive(&json!({}), &c).unwrap();
        let card = &r[0];
        assert_eq!(card["current_phase"], Value::Null);
        assert_eq!(card["last_event_ts"], Value::Null);
        assert_eq!(card["last_actor"], Value::Null);
        assert_eq!(card["last_operation"], Value::Null);
    }

    #[test]
    fn failure_fields_surface_but_superseded_and_null_at_are_excluded() {
        let c = make_db();
        c.execute_batch(
            "INSERT INTO cards (slug, title, status, updated_at) VALUES \
               ('D-1', 'd', 'doing', 't0'), ('D-2', 'd2', 'doing', 't0'), ('D-3', 'd3', 'doing', 't0');",
        ).unwrap();
        c.execute_batch(
            "INSERT INTO contracts (card_slug, last_failure_kind, last_failure_at, superseded_by) VALUES \
               ('D-1', 'qa.fail', '2026-01-01 00:00:00', NULL), \
               ('D-2', 'old.fail', '2025-12-20 00:00:00', 'SUPERSEDED-BY-SOMETHING'), \
               ('D-3', 'no.timestamp', NULL, NULL);",
        ).unwrap();
        let r = cardsActive(&json!({}), &c).unwrap();
        let arr = r.as_array().unwrap();
        let by_slug = |s: &str| arr.iter().find(|c| c["slug"] == json!(s)).unwrap();
        assert_eq!(by_slug("D-1")["last_failure_kind"], json!("qa.fail"));
        assert_eq!(by_slug("D-1")["last_failure_at"], json!("2026-01-01 00:00:00"));
        assert_eq!(by_slug("D-2")["last_failure_kind"], Value::Null); // superseded → excluded
        assert_eq!(by_slug("D-3")["last_failure_kind"], Value::Null); // last_failure_at IS NULL → excluded
    }

    #[test]
    fn body_json_kind_size_extraction_including_double_encoded() {
        let c = make_db();
        c.execute_batch(
            "INSERT INTO cards (slug, title, status, body_json, updated_at) VALUES \
               ('D-1', 'd', 'doing', '{\"kind\":\"feat\",\"size\":\"M\",\"why\":\"w\"}', 't0'), \
               ('D-2', 'd2', 'doing', NULL, 't0'), \
               ('D-3', 'd3', 'doing', 'not valid json{', 't0');",
        ).unwrap();
        c.execute(
            "INSERT INTO cards (slug, title, status, body_json, updated_at) VALUES (?, 'd4', 'doing', ?, 't0')",
            rusqlite::params!["D-4", "\"{\\\"kind\\\":\\\"bug\\\",\\\"size\\\":\\\"S\\\"}\""],
        ).unwrap();
        let r = cardsActive(&json!({}), &c).unwrap();
        let arr = r.as_array().unwrap();
        let by_slug = |s: &str| arr.iter().find(|c| c["slug"] == json!(s)).unwrap();
        assert_eq!(by_slug("D-1")["kind"], json!("feat"));
        assert_eq!(by_slug("D-1")["size"], json!("M"));
        assert_eq!(by_slug("D-2")["kind"], Value::Null);
        assert_eq!(by_slug("D-3")["kind"], Value::Null); // invalid JSON swallowed
        assert_eq!(by_slug("D-4")["kind"], json!("bug")); // double-encoded body reparsed
        assert_eq!(by_slug("D-4")["size"], json!("S"));
    }

    #[test]
    fn missing_contracts_table_yields_null_failure_fields() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT, title TEXT, status TEXT, body_json TEXT, \
               risk INTEGER, complexity INTEGER, priority INTEGER, score INTEGER, driver_intent TEXT, \
               accuracy_score INTEGER, accuracy_computed_at TEXT, awaiting_children_at TEXT, updated_at TEXT); \
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, entity_type TEXT, entity_id TEXT, operation TEXT, ts TEXT, actor TEXT); \
             INSERT INTO cards (slug, title, status, updated_at) VALUES ('D-1', 'd', 'doing', 't0');",
        ).unwrap();
        let r = cardsActive(&json!({}), &c).unwrap();
        assert_eq!(r[0]["last_failure_kind"], Value::Null);
        assert_eq!(r[0]["last_failure_at"], Value::Null);
        assert_eq!(r[0]["slug"], json!("D-1"));
    }
}
