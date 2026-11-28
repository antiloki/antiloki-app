//! inflight-ai-calls.rs — twin of skills/reconcilers/inflight-ai-calls.mjs (reconciler-object
//! lane: the METHODS are the units, both db-last). NAMES ARE CONTRACT: `detect` (detect(db))
//! and `fix` (fix(finding, db)). brain_fires rows older than 5min with no ended_at — likely
//! crashed AI worker; safety=manual, so fix never applies.
use rusqlite::Connection;
use serde_json::{json, Value};

// JS `${v}` for the fire-id/started_at interpolations (string/number/null render like JS).
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
// JS `${v ?? '?'}` — db row values are null (never undefined), so only Null falls back.
fn tmpl_qmark(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "?".into(),
        Some(x) => tmpl(Some(x)),
    }
}

/// JS: `async detect(db)` — stuck brain.brain_fires (no ended_at, started >5min ago), newest
/// first, cap 10. The JS `datetime('now', '-5 minutes')` runs under the conform lane's frozen
/// clock, so the twin writes the frozen instant literally. The try/catch around the query maps
/// a missing brain db / brain_fires table to rows = [], exactly like the JS
/// `catch { /* brain_fires absent */ }`.
pub fn detect(db: &Connection) -> Value {
    let rows = match crate::db_json::query_rows(
        db,
        &format!("\n        SELECT id, started_at, card_slug, decided_agent, decided_action, decided_reason\n        FROM brain.brain_fires\n        WHERE ended_at IS NULL\n          AND started_at < datetime({}, '-5 minutes')\n        ORDER BY id DESC LIMIT 10\n      ", crate::clock::now_sql_inner()),
        &[],
    ) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(), // brain_fires absent
    };
    let findings: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "kind": "inflight-ai-call",
                "severity": "drift",
                "message": format!(
                    "fire #{} ({}: {}) started {} but never ended",
                    tmpl(r.get("id")),
                    tmpl_qmark(r.get("decided_agent")),
                    tmpl_qmark(r.get("decided_action")),
                    tmpl(r.get("started_at"))
                ),
                "card_slug": r.get("card_slug").cloned().unwrap_or(Value::Null),
                "detail": {
                    "fire_id": r.get("id").cloned().unwrap_or(Value::Null),
                    "started_at": r.get("started_at").cloned().unwrap_or(Value::Null),
                    "agent": r.get("decided_agent").cloned().unwrap_or(Value::Null),
                    "action": r.get("decided_action").cloned().unwrap_or(Value::Null),
                },
                "suggested_action": "check AI provider health; if worker is gone, manually UPDATE brain.brain_fires SET ended_at",
            })
        })
        .collect();
    json!(findings)
}

/// JS: `async fix()` — manual-only; the JS method takes no params, so both the finding the
/// harness passes (db-last, args:[finding]) and the db are ignored.
pub fn fix(_finding: &Value, _db: &Connection) -> Value {
    json!({ "applied": false, "reason": "manual — operator must verify AI worker state" })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Frozen conform instant is 2026-01-01 00:00:00 → cutoff 2025-12-31 23:55:00.
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; \
            CREATE TABLE brain.brain_fires (id INTEGER PRIMARY KEY, started_at TEXT, ended_at TEXT, card_slug TEXT, decided_agent TEXT, decided_action TEXT, decided_reason TEXT);").unwrap();
        c
    }

    #[test]
    fn absent_table_and_empty_table_return_empty() {
        let bare = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&bare), json!([])); // brain not attached → JS catch → []
        assert_eq!(detect(&db()), json!([]));
    }

    #[test]
    fn only_stuck_fires_surface_with_full_shape() {
        let c = db();
        c.execute_batch("INSERT INTO brain.brain_fires (id, started_at, ended_at, card_slug, decided_agent, decided_action, decided_reason) VALUES \
            (1, '2025-12-31 23:00:00', NULL, 'CARD-A', 'dev', 'dev.card.work', 'picked by driver'), \
            (2, '2025-12-31 23:58:00', NULL, NULL, 'dev', 'dev.card.work', NULL), \
            (3, '2025-12-31 23:00:00', '2025-12-31 23:04:00', 'CARD-B', 'man', 'man.card.add', NULL), \
            (4, '2025-12-31 23:55:00', NULL, NULL, 'dev', 'dev.card.work', NULL);").unwrap();
        // id 2 too fresh, id 3 ended, id 4 exactly at cutoff (strict <) → only id 1
        let f = detect(&c);
        assert_eq!(f, json!([{
            "kind": "inflight-ai-call",
            "severity": "drift",
            "message": "fire #1 (dev: dev.card.work) started 2025-12-31 23:00:00 but never ended",
            "card_slug": "CARD-A",
            "detail": { "fire_id": 1, "started_at": "2025-12-31 23:00:00", "agent": "dev", "action": "dev.card.work" },
            "suggested_action": "check AI provider health; if worker is gone, manually UPDATE brain.brain_fires SET ended_at",
        }]));
    }

    #[test]
    fn null_agent_action_render_as_question_marks_but_empty_string_stays() {
        let c = db();
        c.execute_batch("INSERT INTO brain.brain_fires (id, started_at) VALUES (7, '2025-12-31 22:00:00'); \
            INSERT INTO brain.brain_fires (id, started_at, card_slug, decided_agent, decided_action) VALUES (8, '2025-12-31 22:30:00', 'CARD-C', '', '');").unwrap();
        let f = detect(&c);
        // newest first: id 8 then id 7. JS `'' ?? '?'` keeps '' ('' is not nullish).
        assert_eq!(f[0]["message"], json!("fire #8 (: ) started 2025-12-31 22:30:00 but never ended"));
        assert_eq!(f[1]["message"], json!("fire #7 (?: ?) started 2025-12-31 22:00:00 but never ended"));
        assert_eq!(f[1]["card_slug"], Value::Null);
        assert_eq!(f[1]["detail"], json!({ "fire_id": 7, "started_at": "2025-12-31 22:00:00", "agent": Value::Null, "action": Value::Null }));
    }

    #[test]
    fn newest_first_capped_at_10() {
        let c = db();
        for i in 1..=12 {
            c.execute(
                "INSERT INTO brain.brain_fires (id, started_at, decided_agent, decided_action) VALUES (?1, '2025-12-31 23:00:00', 'dev', 'dev.card.work')",
                rusqlite::params![i],
            ).unwrap();
        }
        let f = detect(&c);
        let ids: Vec<Value> = f.as_array().unwrap().iter().map(|x| x["detail"]["fire_id"].clone()).collect();
        assert_eq!(json!(ids), json!([12, 11, 10, 9, 8, 7, 6, 5, 4, 3]));
    }

    #[test]
    fn fix_never_applies() {
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(
            fix(&json!({ "kind": "inflight-ai-call" }), &c),
            json!({ "applied": false, "reason": "manual — operator must verify AI worker state" })
        );
        assert_eq!(fix(&Value::Null, &c), json!({ "applied": false, "reason": "manual — operator must verify AI worker state" }));
    }
}
