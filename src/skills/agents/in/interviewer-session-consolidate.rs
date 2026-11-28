//! interviewer-session-consolidate.rs — twin of skills/agents/in/interviewer-session-consolidate.mjs
//! (db WRITE lane). NAMES ARE CONTRACT: `interviewerSessionConsolidate` (default),
//! `AUTO_COMMIT_THRESHOLD` (0.7), `MAX_COMMITS_PER_SESSION` (3, data consts). Closes out a session:
//! runs interviewer-session-patterns-detect → ranks candidates by confidence desc, caps to
//! MAX_COMMITS_PER_SESSION → per candidate: threshold gate, title-substring dedup against
//! memory.memories, dry_run gate, else commits via memory-add (best-effort: a failed commit is
//! recorded as a skip, never thrown) → marks the session closed (best-effort) → logs a
//! review_pass decision via log-decision (fire-and-forget, return value unused).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

pub const AUTO_COMMIT_THRESHOLD: f64 = 0.7;
pub const MAX_COMMITS_PER_SESSION: usize = 3;
const PROVENANCE: &str = "auto:interviewer-consolidator";

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
// JS `${v}` interpolation (string/number/bool/null/absent).
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() } // JS .slice(0,n) (BMP-equivalent)

// existsSimilarMemory(db, title): title's pre-colon head (<=40 chars) LIKE-matched against
// non-superseded memory.memories titles. Any DB/parse failure is swallowed → false (best-effort).
fn exists_similar_memory(db: &Connection, title: &Value) -> bool {
    if !truthy(Some(title)) { return false; }
    let title_s = str_qq(Some(title));
    let head = take_chars(title_s.split(':').next().unwrap_or(""), 40);
    let like = format!("%{head}%");
    match crate::db_json::query_rows(
        db,
        "SELECT id FROM memory.memories WHERE title LIKE ? AND COALESCE(superseded_by,'') = '' LIMIT 1",
        &[json!(like)],
    ) {
        Ok(v) => v.as_array().map_or(false, |a| !a.is_empty()),
        Err(_) => false,
    }
}

// { ...c, reason } — spread c's own keys first (insertion order preserved), reason appended last.
fn with_reason(c: &Value, reason: &str) -> Value {
    let mut obj = c.as_object().cloned().unwrap_or_default();
    obj.insert("reason".into(), json!(reason));
    Value::Object(obj)
}

pub fn interviewerSessionConsolidate(input: &Value, db: &Connection) -> Result<Value, String> {
    let session_id = input.get("session_id").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&session_id)) {
        return Err("interviewer-session-consolidate: session_id is required".into());
    }
    // `const { dry_run = false } = input ?? {}` then `if (dry_run)` — the default only matters for
    // an absent/undefined key, and truthy(None) is already false, so a plain truthy() read over the
    // raw key reproduces the effective boolean for every input shape (default, explicit false/null,
    // or any truthy value) in one step.
    let dry_run = truthy(input.get("dry_run"));

    let patterns_input = json!({ "session_id": session_id.clone() });
    let patterns = crate::agents_in_interviewer_session_patterns_detect::interviewerSessionPatternsDetect(
        &patterns_input,
        db,
    )?;
    let candidates: Vec<Value> = patterns.get("candidates").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    // Sort by confidence desc (stable — ties keep patterns-detect's original emission order:
    // topics, then preferences, then cancellations, then repeats), cap to MAX.
    let mut ranked: Vec<Value> = candidates.clone();
    ranked.sort_by(|a, b| {
        let af = a.get("confidence").and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
        let bf = b.get("confidence").and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
        bf.partial_cmp(&af).unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked.truncate(MAX_COMMITS_PER_SESSION);

    let mut committed: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();

    for c in &ranked {
        let confidence = c.get("confidence").and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
        if confidence < AUTO_COMMIT_THRESHOLD {
            skipped.push(with_reason(c, "below threshold"));
            continue;
        }
        let title = c.get("title").cloned().unwrap_or(Value::Null);
        if exists_similar_memory(db, &title) {
            skipped.push(with_reason(c, "similar memory already exists"));
            continue;
        }
        if dry_run {
            skipped.push(with_reason(c, "dry_run"));
            continue;
        }
        let kind = c.get("kind").cloned().unwrap_or(Value::Null);
        let title_s = title.as_str().unwrap_or("");
        let guidance_s = c.get("guidance").and_then(|v| v.as_str()).unwrap_or("");
        let mem_input = json!({
            "type": c.get("type").cloned().unwrap_or(Value::Null),
            "title": take_chars(title_s, 200),
            "situation": format!("Auto-extracted from interviewer session #{}.", tmpl(Some(&session_id))),
            "guidance": take_chars(guidance_s, 1000),
            "rationale": format!("Pattern: {} (confidence={:.2}).", tmpl(Some(&kind)), confidence),
            "tags": ["auto-extracted", "interviewer-consolidator", format!("pattern:{}", tmpl(Some(&kind)))],
            "confidence": confidence,
            "created_by": PROVENANCE,
        });
        match crate::entities_memory_add::memoryAdd(&mem_input, db) {
            Ok(mem) => {
                // { memory_id: mem.id, ...c } — memory_id first, then c's own keys in order.
                let mut obj = Map::new();
                obj.insert("memory_id".into(), mem.get("id").cloned().unwrap_or(Value::Null));
                if let Value::Object(cm) = c {
                    for (k, v) in cm { obj.insert(k.clone(), v.clone()); }
                }
                committed.push(Value::Object(obj));
            }
            // Best-effort: a memory-add failure (validation, constraint) is recorded as a skip, not
            // propagated. Only reachable via a genuinely unexpected write failure — every candidate
            // patterns-detect emits is well-formed for memory-add — so this branch is not fixtured
            // (its message text would be a fragile cross-engine match for an unreachable-in-practice
            // path; mirrors the "guard-throws not exercised" posture for internal catches).
            Err(e) => skipped.push(with_reason(c, &format!("commit failed: {e}"))),
        }
    }

    // Mark session closed (idempotent, best-effort — any failure leaves 'unknown').
    let mut session_status = json!("unknown");
    if crate::db_json::run_write(
        db,
        &format!("UPDATE interviewer_sessions SET status = 'closed', ended_at = COALESCE(ended_at, {}) WHERE id = ?", crate::clock::now_sql()),
        &[session_id.clone()],
    ).is_ok()
    {
        if let Ok(rows) = crate::db_json::query_rows(db, "SELECT status FROM interviewer_sessions WHERE id = ?", &[session_id.clone()]) {
            if let Some(row) = rows.as_array().and_then(|a| a.first()) {
                let s = row.get("status").cloned().unwrap_or(Value::Null);
                session_status = if s.is_null() { json!("unknown") } else { s };
            }
        }
    }

    // Audit-style decision row for traceability. log-decision swallows its own failures and returns
    // null either way — the return value is never used by the JS source either.
    let log_input = json!({
        "agent": "in",
        "decision": "review_pass",
        "rationale": format!(
            "consolidated session #{}: {} committed, {} skipped ({} candidates from patterns-detect)",
            tmpl(Some(&session_id)), committed.len(), skipped.len(), candidates.len()
        ),
        "inputs": {
            "session_id": session_id.clone(),
            "candidates_count": candidates.len() as i64,
            "committed_count": committed.len() as i64,
        },
    });
    let _ = crate::engine_log_decision::logDecision(&log_input, db);

    let mut out = Map::new();
    out.insert("session_id".into(), session_id);
    out.insert("candidates_total".into(), json!(candidates.len() as i64));
    out.insert("committed".into(), json!(committed));
    out.insert("skipped".into(), json!(skipped));
    out.insert("session_status".into(), session_status);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "
        CREATE TABLE interviewer_sessions (id TEXT PRIMARY KEY, status TEXT, ended_at TEXT);
        CREATE TABLE interviewer_turns (session_id TEXT, turn_index INTEGER, user_text TEXT, classified_kind TEXT, proposed_actions_json TEXT, confirmed INTEGER, dispatched_action_ids_json TEXT);
        CREATE TABLE memory.memories (
          id              INTEGER PRIMARY KEY AUTOINCREMENT,
          type            TEXT    NOT NULL,
          title           TEXT    NOT NULL,
          situation       TEXT    NOT NULL,
          guidance        TEXT    NOT NULL,
          naive_response  TEXT,
          rationale       TEXT,
          tags_json       TEXT    NOT NULL DEFAULT '[]',
          source_card     TEXT,
          source_event    INTEGER,
          source_session  TEXT,
          confidence      INTEGER NOT NULL DEFAULT 0,
          validated       INTEGER NOT NULL DEFAULT 0,
          superseded_by   INTEGER,
          agent_slug      TEXT,
          created_at      TEXT    NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
          created_by      TEXT    NOT NULL,
          CHECK (type IN ('lesson', 'preference', 'decision', 'reference', 'correction', 'narrative')),
          CHECK (confidence >= 0 AND confidence <= 1),
          CHECK (validated IN (0, 1))
        );
        CREATE TABLE brain.brain_decisions ( id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, decided_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), card_slug TEXT, decision TEXT NOT NULL, rationale TEXT NOT NULL, inputs_json TEXT, fire_id INTEGER, CHECK (decision IN ('fire','wait','rollback','escalate','done','pause','session_start','session_end','goal_set','classify','gate_verdict','rule_match','sandbox_create','review_pass','schedule_and_dispatch','agent_step')) );
    ";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory; ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }
    fn turn(c: &Connection, sid: &str, idx: i64, text: &str, proposed: Option<&str>, confirmed: i64, dispatched: Option<&str>) {
        c.execute(
            "INSERT INTO interviewer_turns (session_id, turn_index, user_text, classified_kind, proposed_actions_json, confirmed, dispatched_action_ids_json) VALUES (?1,?2,?3,'chat',?4,?5,?6)",
            rusqlite::params![sid, idx, text, proposed, confirmed, dispatched],
        ).unwrap();
    }
    fn session(c: &Connection, id: &str) {
        c.execute("INSERT INTO interviewer_sessions (id, status, ended_at) VALUES (?1,'active',NULL)", rusqlite::params![id]).unwrap();
    }

    #[test]
    fn requires_session_id() {
        let c = db();
        assert!(interviewerSessionConsolidate(&json!({}), &c).is_err());
    }

    #[test]
    fn below_threshold_is_skipped_but_session_still_closes() {
        let c = db();
        session(&c, "S-BELOW");
        for i in 0..3 { turn(&c, "S-BELOW", i, "sandbox note", None, 1, None); }
        turn(&c, "S-BELOW", 3, "filler four", None, 1, None);
        turn(&c, "S-BELOW", 4, "filler five", None, 1, None);
        let r = interviewerSessionConsolidate(&json!({ "session_id": "S-BELOW" }), &c).unwrap();
        assert_eq!(r["candidates_total"], json!(1));
        assert_eq!(r["committed"], json!([]));
        assert_eq!(r["skipped"][0]["reason"], json!("below threshold"));
        assert_eq!(r["session_status"], json!("closed"));
    }

    #[test]
    fn dry_run_skips_without_writing() {
        let c = db();
        session(&c, "S-DRY");
        turn(&c, "S-DRY", 0, "I always want fewer clicks", None, 1, None);
        for i in 1..5 { turn(&c, "S-DRY", i, "filler note", None, 1, None); }
        let r = interviewerSessionConsolidate(&json!({ "session_id": "S-DRY", "dry_run": true }), &c).unwrap();
        assert_eq!(r["committed"], json!([]));
        assert_eq!(r["skipped"][0]["reason"], json!("dry_run"));
        let rows = crate::db_json::query_rows(&c, "SELECT COUNT(*) AS n FROM memory.memories", &[]).unwrap();
        assert_eq!(rows[0]["n"], json!(0));
    }

    #[test]
    fn caps_committed_at_max_and_drops_the_overflow_candidate() {
        let c = db();
        session(&c, "S-CAP");
        // TOPIC (gate, n=4 -> 0.7)
        for i in 0..4 { turn(&c, "S-CAP", i, "gate note", None, 1, None); }
        // PREFERENCE (0.85, highest -> always kept)
        turn(&c, "S-CAP", 4, "I prefer bright icons", None, 1, None);
        // CANCEL (custom.cancel.test, n=2 -> 0.7)
        turn(&c, "S-CAP", 5, "try the risky operation", Some("[{\"action\":\"custom.cancel.test\",\"risk_tier\":\"medium\"}]"), 0, None);
        turn(&c, "S-CAP", 6, "attempt it once more now", Some("[{\"action\":\"custom.cancel.test\",\"risk_tier\":\"medium\"}]"), 0, None);
        // REPEAT (recheck status, group=2 -> 0.7) — pushed out by the cap
        turn(&c, "S-CAP", 7, "recheck status", None, 1, Some("[\"z1\"]"));
        turn(&c, "S-CAP", 8, "recheck status", None, 1, Some("[\"z2\"]"));
        let r = interviewerSessionConsolidate(&json!({ "session_id": "S-CAP" }), &c).unwrap();
        assert_eq!(r["candidates_total"], json!(4));
        let committed = r["committed"].as_array().unwrap();
        assert_eq!(committed.len(), MAX_COMMITS_PER_SESSION);
        assert_eq!(r["skipped"], json!([]));
        let kinds: Vec<&str> = committed.iter().map(|x| x["kind"].as_str().unwrap()).collect();
        assert!(!kinds.contains(&"SUCCESSFUL_REPEAT"), "the lowest-ranked tie must be cut by the cap");
        assert!(committed[0]["memory_id"].is_number());
    }

    #[test]
    fn preseeded_similar_title_blocks_the_commit() {
        let c = db();
        session(&c, "S-DUP");
        c.execute(
            "INSERT INTO memory.memories (type,title,situation,guidance,created_by,confidence) VALUES ('preference','User preference: something old','seed','seed','seed-fixture',0.5)",
            [],
        ).unwrap();
        turn(&c, "S-DUP", 0, "I prefer dark mode", None, 1, None);
        for i in 1..5 { turn(&c, "S-DUP", i, "checking things", None, 1, None); }
        let r = interviewerSessionConsolidate(&json!({ "session_id": "S-DUP" }), &c).unwrap();
        assert_eq!(r["committed"], json!([]));
        assert_eq!(r["skipped"][0]["reason"], json!("similar memory already exists"));
    }

    #[test]
    fn within_batch_self_collision_blocks_the_second_topic() {
        let c = db();
        session(&c, "S-SELFDUP");
        for i in 0..5 { turn(&c, "S-SELFDUP", i, "audit note", None, 1, None); } // n=5 -> 0.8
        for i in 5..9 { turn(&c, "S-SELFDUP", i, "rag note", None, 1, None); } // n=4 -> 0.7
        let r = interviewerSessionConsolidate(&json!({ "session_id": "S-SELFDUP" }), &c).unwrap();
        assert_eq!(r["committed"].as_array().unwrap().len(), 1);
        assert_eq!(r["committed"][0]["evidence"]["stem"], json!("audit"));
        assert_eq!(r["skipped"][0]["reason"], json!("similar memory already exists"));
        assert_eq!(r["skipped"][0]["evidence"]["stem"], json!("rag"));
    }

    #[test]
    fn nonexistent_session_row_leaves_status_unknown() {
        let c = db();
        for i in 0..5 { turn(&c, "S-NOROW", i, "morning check", None, 1, None); }
        let r = interviewerSessionConsolidate(&json!({ "session_id": "S-NOROW" }), &c).unwrap();
        assert_eq!(r["candidates_total"], json!(0));
        assert_eq!(r["session_status"], json!("unknown"));
    }
}
