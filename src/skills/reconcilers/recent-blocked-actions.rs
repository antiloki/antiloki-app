//! recent-blocked-actions.rs — twin of skills/reconcilers/recent-blocked-actions.mjs
//! (reconciler-object lane: the METHODS are the units, both db-last). NAMES ARE CONTRACT:
//! `detect` (detect(db)) and `fix` (fix(finding, db)). Actions blocked by law-hooks in the
//! last 24h — each row is a deliberate substrate denial; safety=manual, fix never applies.
//! The JS datetime('now', '-1 day') is written frozen — datetime('2026-01-01 00:00:00', '-1 day')
//! — to match the conform JS lane's frozen clock (window floor: 2025-12-31 00:00:00).

use rusqlite::Connection;
use serde_json::{json, Value};

fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand

/// JS: `async detect(db)` — blocked law_violations from the last 24h, newest first, max 10.
/// The try/catch around the query maps ANY prepare error (laws not attached / law_violations
/// table absent on legacy DBs) to rows = [], exactly like the JS `catch { }`.
pub fn detect(db: &Connection) -> Value {
    let rows = match crate::db_json::query_rows(
        db,
        &format!("\n        SELECT id, attempted_at AS ts, action_name, agent_name, law_slug, reason\n        FROM laws.law_violations\n        WHERE outcome = 'blocked'\n          AND attempted_at >= datetime({}, '-1 day')\n        ORDER BY id DESC LIMIT 10\n      ", crate::clock::now_sql_inner()),
        &[],
    ) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(), // law_violations table absent on legacy DBs
    };
    let findings: Vec<Value> = rows
        .iter()
        .map(|r| {
            // `${r.agent_name ?? '?'}` — '' stays '' (nullish, not falsy); null/undefined → '?'
            let agent = match nz(r.get("agent_name")) { Some(v) => tmpl(Some(v)), None => "?".into() };
            // `${(r.reason ?? '').slice(0, 80)}`
            let reason: String = str_qq(r.get("reason")).chars().take(80).collect();
            // strict === against the string — only an exact string match takes the special branch
            let suggested_action = if r.get("law_slug").and_then(|v| v.as_str()) == Some("COMMIT_CODE_BEFORE_AUDIT_COMMIT") {
                "run uncommitted-dev-output reconciler OR git commit drift manually"
            } else {
                "inspect the law (system.law.find) — fix the root cause OR grant the permission"
            };
            json!({
                "kind": "recent-blocked-action",
                "severity": "drift",
                "message": format!(
                    "{} by {} blocked at {}: {}",
                    tmpl(r.get("action_name")), agent, tmpl(r.get("ts")), reason
                ),
                "detail": {
                    "law_violation_id": r.get("id").cloned().unwrap_or(Value::Null),
                    "action_name": r.get("action_name").cloned().unwrap_or(Value::Null),
                    "agent_name": r.get("agent_name").cloned().unwrap_or(Value::Null),
                    "law_slug": r.get("law_slug").cloned().unwrap_or(Value::Null),
                },
                "suggested_action": suggested_action,
            })
        })
        .collect();
    json!(findings)
}

/// JS: `async fix()` — manual-only; the JS method takes no params, so both the finding the
/// harness passes (db-last, args:[finding]) and the db are ignored.
pub fn fix(_finding: &Value, _db: &Connection) -> Value {
    json!({ "applied": false, "reason": "manual — operator must decide per-case" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH DATABASE ':memory:' AS laws; \
             CREATE TABLE laws.law_violations (id INTEGER PRIMARY KEY, attempted_at TEXT, action_name TEXT, agent_name TEXT, law_slug TEXT, reason TEXT, outcome TEXT)",
        )
        .unwrap();
        c
    }

    fn insert(c: &Connection, id: i64, at: &str, action: &str, agent: Option<&str>, law: Option<&str>, reason: Option<&str>, outcome: &str) {
        c.execute(
            "INSERT INTO laws.law_violations (id, attempted_at, action_name, agent_name, law_slug, reason, outcome) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![id, at, action, agent, law, reason, outcome],
        )
        .unwrap();
    }

    #[test]
    fn absent_table_and_empty_table_return_empty() {
        let bare = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&bare), json!([])); // laws not attached → JS catch → []
        assert_eq!(detect(&db()), json!([]));
    }

    #[test]
    fn window_outcome_filter_and_full_shape_newest_first() {
        let c = db();
        insert(&c, 1, "2025-12-31 12:00:00", "dev.file.write", Some("dev"), Some("NEVER_COMMIT_SECRETS"), Some("attempted to write .env into a commit"), "blocked");
        insert(&c, 2, "2025-12-31 00:00:00", "man.card.add", Some("man"), Some("CARD_BODY_MUST_MATCH_SCHEMA"), Some("body_json rejected"), "blocked"); // boundary: >= includes it
        insert(&c, 3, "2025-12-30 23:59:59", "dev.file.write", Some("dev"), Some("NEVER_COMMIT_SECRETS"), Some("too old"), "blocked"); // outside 24h window
        insert(&c, 4, "2025-12-31 14:00:00", "dev.file.write", Some("dev"), Some("NEVER_COMMIT_SECRETS"), Some("only warned"), "warned"); // outcome filtered
        let f = detect(&c);
        // ORDER BY id DESC → id 2 (boundary row) first, then id 1
        assert_eq!(f, json!([
            {
                "kind": "recent-blocked-action",
                "severity": "drift",
                "message": "man.card.add by man blocked at 2025-12-31 00:00:00: body_json rejected",
                "detail": { "law_violation_id": 2, "action_name": "man.card.add", "agent_name": "man", "law_slug": "CARD_BODY_MUST_MATCH_SCHEMA" },
                "suggested_action": "inspect the law (system.law.find) — fix the root cause OR grant the permission",
            },
            {
                "kind": "recent-blocked-action",
                "severity": "drift",
                "message": "dev.file.write by dev blocked at 2025-12-31 12:00:00: attempted to write .env into a commit",
                "detail": { "law_violation_id": 1, "action_name": "dev.file.write", "agent_name": "dev", "law_slug": "NEVER_COMMIT_SECRETS" },
                "suggested_action": "inspect the law (system.law.find) — fix the root cause OR grant the permission",
            },
        ]));
    }

    #[test]
    fn order_is_id_desc() {
        let c = db();
        insert(&c, 1, "2025-12-31 12:00:00", "a.b", Some("dev"), None, None, "blocked");
        insert(&c, 2, "2025-12-31 13:00:00", "c.d", Some("dev"), None, None, "blocked");
        let f = detect(&c);
        let ids: Vec<Value> = f.as_array().unwrap().iter().map(|x| x["detail"]["law_violation_id"].clone()).collect();
        assert_eq!(json!(ids), json!([2, 1]));
    }

    #[test]
    fn null_agent_reason_law_and_slice_and_commit_branch() {
        let c = db();
        let long = "r".repeat(100);
        insert(&c, 5, "2025-12-31 15:00:00", "system.audit.commit", None, Some("COMMIT_CODE_BEFORE_AUDIT_COMMIT"), None, "blocked");
        insert(&c, 6, "2025-12-31 16:00:00", "man.card.add", Some("man"), None, Some(&long), "blocked");
        insert(&c, 7, "2025-12-31 17:00:00", "x.y", Some(""), Some("GOD_MODE_TTY_REQUIRED"), Some(""), "blocked");
        let f = detect(&c);
        // id 7: '' agent is NOT nullish → stays '' (not '?')
        assert_eq!(f[0]["message"], json!("x.y by  blocked at 2025-12-31 17:00:00: "));
        // id 6: reason sliced to 80, null law_slug → generic suggestion
        assert_eq!(f[1]["message"], json!(format!("man.card.add by man blocked at 2025-12-31 16:00:00: {}", "r".repeat(80))));
        assert_eq!(f[1]["detail"]["law_slug"], Value::Null);
        assert_eq!(f[1]["suggested_action"], json!("inspect the law (system.law.find) — fix the root cause OR grant the permission"));
        // id 5: null agent → '?', null reason → '', COMMIT_CODE branch
        assert_eq!(f[2]["message"], json!("system.audit.commit by ? blocked at 2025-12-31 15:00:00: "));
        assert_eq!(f[2]["detail"], json!({ "law_violation_id": 5, "action_name": "system.audit.commit", "agent_name": Value::Null, "law_slug": "COMMIT_CODE_BEFORE_AUDIT_COMMIT" }));
        assert_eq!(f[2]["suggested_action"], json!("run uncommitted-dev-output reconciler OR git commit drift manually"));
    }

    #[test]
    fn newest_first_capped_at_10() {
        let c = db();
        for i in 1..=12 {
            insert(&c, i, "2025-12-31 12:00:00", "a.b", Some("dev"), None, None, "blocked");
        }
        let f = detect(&c);
        let ids: Vec<Value> = f.as_array().unwrap().iter().map(|x| x["detail"]["law_violation_id"].clone()).collect();
        assert_eq!(json!(ids), json!([12, 11, 10, 9, 8, 7, 6, 5, 4, 3]));
    }

    #[test]
    fn fix_never_applies() {
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(
            fix(&json!({ "kind": "recent-blocked-action" }), &c),
            json!({ "applied": false, "reason": "manual — operator must decide per-case" })
        );
        assert_eq!(fix(&Value::Null, &c), json!({ "applied": false, "reason": "manual — operator must decide per-case" }));
    }
}
