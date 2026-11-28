//! interviewer-context-bundle.rs — twin of skills/agents/in/interviewer-context-bundle.mjs (db READ
//! lane). NAMES ARE CONTRACT: `interviewerContextBundle` (default). Assembles the multi-DB context
//! snapshot the Interviewer needs to classify the user's next turn — pure read across main + memory
//! + brain. `safe()` mirrors the JS try/catch: any failed query (missing table, absent ATTACH)
//! collapses to []. The session lookup is deliberately NOT safe()-wrapped, matching the JS.
// QUALITY: 82/100 — faithful safe()-swallow + .get()??null via first-row-or-null; verbatim SQL
use rusqlite::Connection;
use serde_json::{json, Map, Value};

const TIMELINE_LIMIT: i64 = 10;
const CARDS_LIMIT: i64 = 5;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }

// JS `safe(db, sql, params = [])` — try { db.prepare(sql).all(...params) } catch { return [] }
fn safe(db: &Connection, sql: &str, params: &[Value]) -> Value {
    crate::db_json::query_rows(db, sql, params).unwrap_or_else(|_| json!([]))
}

pub fn interviewerContextBundle(input: &Value, db: &Connection) -> Result<Value, String> {
    let session_id = input.get("session_id");

    let mut session = Value::Null;
    if truthy(session_id) {
        let sid = session_id.cloned().unwrap_or(Value::Null);
        // NOT safe()-wrapped in the JS: a missing interviewer_sessions table throws through.
        let rows = crate::db_json::query_rows(db, "\n      SELECT id, started_at, user_identifier, status,\n             (SELECT COUNT(*) FROM interviewer_turns WHERE session_id = ?) AS turn_count\n      FROM interviewer_sessions WHERE id = ?\n    ", &[sid.clone(), sid])?;
        // .get(...) returns first row or undefined; `?? null` → null
        session = rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
    }

    let mut out = Map::new();
    out.insert("session".into(), session);
    out.insert("active_cards".into(), safe(db, "\n      SELECT slug, title, status, updated_at FROM cards\n      WHERE status NOT IN ('done','blocked')\n      ORDER BY updated_at DESC LIMIT ?", &[json!(CARDS_LIMIT)]));
    out.insert("user_preferences".into(), safe(db, "\n      SELECT id, title, guidance FROM memory.memories\n      WHERE type='preference' AND validated=1 AND superseded_by IS NULL", &[]));
    out.insert("recent_corrections".into(), safe(db, "\n      SELECT id, title, situation, guidance FROM memory.memories\n      WHERE type='correction' AND superseded_by IS NULL ORDER BY id DESC LIMIT 5", &[]));
    out.insert("recent_timeline".into(), safe(db, "\n      SELECT ts, actor, operation, entity_type, entity_id\n      FROM sacred_timeline ORDER BY id DESC LIMIT ?", &[json!(TIMELINE_LIMIT)]));
    out.insert("pending_pl_sessions".into(), safe(db, "\n      SELECT id, status, raw_intent, approved_card_slug FROM pl_sessions\n      WHERE status NOT IN ('done','abandoned')", &[]));
    out.insert("pending_qa".into(), safe(db, "\n      SELECT slug, card_slug, status FROM contracts WHERE status='qa'", &[]));
    out.insert("open_fires".into(), safe(db, "\n      SELECT id, agent, action, fired_at FROM brain.brain_fires\n      WHERE ended_at IS NULL ORDER BY fired_at DESC LIMIT 5", &[]));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory; ATTACH ':memory:' AS brain; \
          CREATE TABLE interviewer_sessions (id TEXT PRIMARY KEY, started_at TEXT, user_identifier TEXT, status TEXT); \
          CREATE TABLE interviewer_turns (id INTEGER PRIMARY KEY, session_id TEXT); \
          CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, updated_at TEXT); \
          CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, type TEXT, title TEXT, situation TEXT, guidance TEXT, validated INTEGER, superseded_by INTEGER); \
          CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT); \
          CREATE TABLE pl_sessions (id INTEGER PRIMARY KEY, status TEXT, raw_intent TEXT, approved_card_slug TEXT); \
          CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT, status TEXT); \
          CREATE TABLE brain.brain_fires (id INTEGER PRIMARY KEY, agent TEXT, action TEXT, fired_at TEXT, ended_at TEXT);").unwrap();
        c
    }

    #[test]
    fn bundles_full_snapshot_with_session() {
        let c = full_db();
        c.execute_batch("INSERT INTO interviewer_sessions VALUES ('S1','2025-12-30 09:00:00','danil','active'); \
          INSERT INTO interviewer_turns (session_id) VALUES ('S1'),('S1'),('S2'); \
          INSERT INTO cards VALUES ('C1','one','doing','2025-12-01 10:00:00'),('C2','two','done','2025-12-09 10:00:00'),('C3','three','backlog','2025-12-03 10:00:00'),('C4','four','blocked','2025-12-08 10:00:00'),('C5','five','review','2025-12-05 10:00:00'),('C6','six','doing','2025-12-06 10:00:00'),('C7','seven','backlog','2025-12-07 10:00:00'),('C8','eight','doing','2025-12-02 10:00:00'); \
          INSERT INTO memory.memories VALUES (1,'preference','pref A',NULL,'do A',1,NULL),(2,'preference','pref B',NULL,'do B',0,NULL),(3,'correction','corr A','sit A','fix A',1,NULL),(4,'correction','corr old','sit','fix',1,9); \
          INSERT INTO sacred_timeline VALUES (1,'t1','dev','card.add','card','C1'),(2,'t2','man','card.update','card','C1'); \
          INSERT INTO pl_sessions VALUES (1,'active','build X',NULL),(2,'done','shipped','C1'),(3,'abandoned','meh',NULL); \
          INSERT INTO contracts VALUES ('K1','C1','qa'),('K2','C1','draft'); \
          INSERT INTO brain.brain_fires VALUES (1,'dev','tick','2025-12-31 08:00:00',NULL),(2,'dev','tick','2025-12-31 09:00:00','2025-12-31 09:05:00');").unwrap();
        let r = interviewerContextBundle(&json!({"session_id":"S1"}), &c).unwrap();
        assert_eq!(r["session"]["turn_count"], json!(2)); // only S1's turns
        assert_eq!(r["session"]["user_identifier"], json!("danil"));
        let cards = r["active_cards"].as_array().unwrap();
        assert_eq!(cards.len(), 5); // 6 active, LIMIT 5
        assert_eq!(cards[0]["slug"], json!("C7")); // updated_at DESC; done/blocked excluded
        assert_eq!(cards[4]["slug"], json!("C8")); // C1 (oldest active) cut by LIMIT
        let prefs = r["user_preferences"].as_array().unwrap();
        assert_eq!(prefs.len(), 1); // validated=1 + not superseded only
        assert_eq!(prefs[0]["guidance"], json!("do A"));
        let corr = r["recent_corrections"].as_array().unwrap();
        assert_eq!(corr.len(), 1); // superseded correction excluded
        assert_eq!(corr[0]["situation"], json!("sit A"));
        assert_eq!(r["recent_timeline"][0]["entity_id"], json!("C1")); // id DESC → id=2 first
        assert_eq!(r["recent_timeline"][0]["operation"], json!("card.update"));
        assert_eq!(r["pending_pl_sessions"].as_array().unwrap().len(), 1); // done/abandoned excluded
        assert_eq!(r["pending_qa"], json!([{"slug":"K1","card_slug":"C1","status":"qa"}]));
        let fires = r["open_fires"].as_array().unwrap();
        assert_eq!(fires.len(), 1); // ended_at IS NULL only
        assert_eq!(fires[0]["id"], json!(1));
    }

    #[test]
    fn no_session_and_missing_tables_yield_null_and_empties() {
        let bare = Connection::open_in_memory().unwrap(); // NO tables → every safe() catches → []
        let r = interviewerContextBundle(&json!({}), &bare).unwrap();
        assert_eq!(r["session"], Value::Null);
        for k in ["active_cards","user_preferences","recent_corrections","recent_timeline","pending_pl_sessions","pending_qa","open_fires"] {
            assert_eq!(r[k], json!([]), "{k} defaults to []");
        }
        // falsy session_id ('' / absent) skips the lookup even on a bare db (no throw)
        let r_empty = interviewerContextBundle(&json!({"session_id":""}), &bare).unwrap();
        assert_eq!(r_empty["session"], Value::Null);
        // unknown session id on a real schema → .get() ?? null
        let c = full_db();
        let r2 = interviewerContextBundle(&json!({"session_id":"NOPE"}), &c).unwrap();
        assert_eq!(r2["session"], Value::Null);
    }
}
