//! unread-error-inbox.rs — twin of skills/reconcilers/unread-error-inbox.mjs (reconciler-object
//! lane). NAMES ARE CONTRACT: the units are the default object's METHODS `detect(db)` and
//! `fix(finding, db)`. Unread severity='error' inbox messages surfaced as Fixit findings;
//! safety is manual — fix() only tells the operator to pick option A (read) or B (archive).

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`

// The JS findings carry fix_options whose `fix` values are FUNCTIONS. The conform JS lane's
// normalizeResult stringifies a function via String(v) — i.e. Function.prototype.toString(),
// the exact source slice from the .mjs file. antiloki-core sets eol=lf in .gitattributes, so that
// slice uses \n. These literals were transcribed while the .mjs files were still CRLF on disk and
// silently diverged once the repo normalized — conform-db caught it. They must stay byte-identical
// to the JS source (12-space body indent, 10-space closing brace, \n endings).
const FIX_A_SRC: &str = "async fix(finding, db2) {\n            const id = finding.detail?.inbox_id;\n            if (!id) return { applied: false, reason: 'missing inbox_id' };\n            const r = db2.prepare(`UPDATE inbox_messages SET read_at = datetime('now') WHERE id = ?`).run(id);\n            return { applied: r.changes > 0, changes: { inbox_id: id, marked_read: true } };\n          }";
const FIX_B_SRC: &str = "async fix(finding, db2) {\n            const id = finding.detail?.inbox_id;\n            if (!id) return { applied: false, reason: 'missing inbox_id' };\n            const r = db2.prepare(`UPDATE inbox_messages SET archived_at=datetime('now'), read_at=COALESCE(read_at, datetime('now')) WHERE id = ?`).run(id);\n            return { applied: r.changes > 0, changes: { inbox_id: id, archived: true } };\n          }";

/// JS: `detect(db)` — unread + unarchived severity='error' inbox rows, newest first, max 10.
/// The try/catch around the query maps ANY prepare error (inbox_messages absent) to [] exactly
/// like the JS `catch { /* table absent */ }` leaving rows = [].
pub fn detect(db: &Connection) -> Value {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, ts, kind, subject, related_card_slug, suggested_reconciler, suggested_card_action
         FROM inbox_messages
         WHERE severity = 'error'
           AND read_at IS NULL
           AND archived_at IS NULL
         ORDER BY id DESC LIMIT 10",
        &[],
    )
    .ok()
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();

    let findings: Vec<Value> = rows
        .iter()
        .map(|r| {
            // (r.subject ?? '').slice(0, 100)
            let subject_sliced: String = str_qq(r.get("subject")).chars().take(100).collect();
            let sr = r.get("suggested_reconciler");
            let suggested_action = if truthy(sr) {
                format!("open Inbox (i) → expand for context → press f to run reconciler '{}'", tmpl(sr))
            } else {
                "open Inbox (i) to read the full body + suggested action".to_string()
            };

            let mut detail = Map::new();
            detail.insert("inbox_id".into(), r.get("id").cloned().unwrap_or(Value::Null));
            detail.insert("ts".into(), r.get("ts").cloned().unwrap_or(Value::Null));
            detail.insert("suggested_reconciler".into(), sr.cloned().unwrap_or(Value::Null));
            detail.insert("suggested_card_action".into(), r.get("suggested_card_action").cloned().unwrap_or(Value::Null));

            let mut f = Map::new();
            f.insert("kind".into(), json!("unread-error-inbox"));
            f.insert("severity".into(), json!("human_blocking"));
            f.insert("message".into(), json!(format!("Inbox #{} ({}): {}", tmpl(r.get("id")), tmpl(r.get("kind")), subject_sliced)));
            f.insert("card_slug".into(), r.get("related_card_slug").cloned().unwrap_or(Value::Null));
            f.insert("detail".into(), Value::Object(detail));
            f.insert("suggested_action".into(), json!(suggested_action));
            f.insert("confidence".into(), json!(0.5));
            f.insert("fix_options".into(), json!([
                { "key": "A", "label": "Mark this email as read (acknowledge, take action via Inbox)", "fix": FIX_A_SRC },
                { "key": "B", "label": "Archive this email (handled or irrelevant)", "fix": FIX_B_SRC },
            ]));
            Value::Object(f)
        })
        .collect();
    json!(findings)
}

/// JS: `async fix() { ... }` — manual reconciler: ignores its (finding, db) args and returns
/// the static "pick A or B" refusal. Never touches the db.
pub fn fix(_finding: &Value, _db: &Connection) -> Value {
    json!({ "applied": false, "reason": "manual — pick A (read) or B (archive)" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE inbox_messages (id INTEGER PRIMARY KEY, ts TEXT, kind TEXT, subject TEXT, severity TEXT, read_at TEXT, archived_at TEXT, related_card_slug TEXT, suggested_reconciler TEXT, suggested_card_action TEXT)").unwrap();
        c
    }

    #[test]
    fn absent_table_and_empty_table_yield_empty_array() {
        let empty = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&empty), json!([])); // table absent → JS catch → []
        assert_eq!(detect(&db()), json!([]));
    }

    #[test]
    fn surfaces_unread_errors_newest_first_with_full_shape() {
        let c = db();
        c.execute_batch("INSERT INTO inbox_messages (id, ts, kind, subject, severity, related_card_slug, suggested_reconciler, suggested_card_action) VALUES (1, '2025-12-30 10:00:00', 'driver-tick-failed', 'driver tick failed hard', 'error', 'CARD-A', 'stuck-driver', 'card.discuss'); \
            INSERT INTO inbox_messages (id, ts, kind, severity) VALUES (2, '2025-12-30 11:00:00', 'contract-escalated', 'error'); \
            INSERT INTO inbox_messages (id, ts, kind, subject, severity, read_at) VALUES (3, '2025-12-30 12:00:00', 'driver-tick-failed', 'already read', 'error', '2025-12-31 00:00:00'); \
            INSERT INTO inbox_messages (id, ts, kind, subject, severity, archived_at) VALUES (4, '2025-12-30 13:00:00', 'driver-tick-failed', 'archived', 'error', '2025-12-31 00:00:00'); \
            INSERT INTO inbox_messages (id, ts, kind, subject, severity) VALUES (5, '2025-12-30 14:00:00', 'digest', 'not an error', 'info');").unwrap();
        let out = detect(&c);
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 2); // read, archived, info excluded
        // newest first: id 2 (null subject, no reconciler) then id 1 (full row)
        assert_eq!(arr[0]["message"], json!("Inbox #2 (contract-escalated): "));
        assert_eq!(arr[0]["card_slug"], Value::Null);
        assert_eq!(arr[0]["suggested_action"], json!("open Inbox (i) to read the full body + suggested action"));
        assert_eq!(arr[0]["detail"], json!({ "inbox_id": 2, "ts": "2025-12-30 11:00:00", "suggested_reconciler": null, "suggested_card_action": null }));
        assert_eq!(arr[1]["kind"], json!("unread-error-inbox"));
        assert_eq!(arr[1]["severity"], json!("human_blocking"));
        assert_eq!(arr[1]["message"], json!("Inbox #1 (driver-tick-failed): driver tick failed hard"));
        assert_eq!(arr[1]["card_slug"], json!("CARD-A"));
        assert_eq!(arr[1]["suggested_action"], json!("open Inbox (i) → expand for context → press f to run reconciler 'stuck-driver'"));
        assert_eq!(arr[1]["confidence"], json!(0.5));
        assert_eq!(arr[1]["fix_options"][0]["key"], json!("A"));
        assert_eq!(arr[1]["fix_options"][1]["key"], json!("B"));
        // the stringified JS method sources ride along byte-exactly (LF, matching the .mjs on disk)
        assert!(arr[1]["fix_options"][0]["fix"].as_str().unwrap().starts_with("async fix(finding, db2) {\n"));
        assert!(arr[1]["fix_options"][0]["fix"].as_str().unwrap().contains("SET read_at = datetime('now')"));
        assert!(arr[1]["fix_options"][1]["fix"].as_str().unwrap().contains("SET archived_at=datetime('now')"));
    }

    #[test]
    fn long_subject_slices_to_100_and_empty_reconciler_is_falsy() {
        let c = db();
        let long = "x".repeat(150);
        c.execute("INSERT INTO inbox_messages (id, ts, kind, subject, severity, suggested_reconciler) VALUES (7, '2025-12-30 15:00:00', 'contract-escalated', ?1, 'error', '')", rusqlite::params![long]).unwrap();
        let out = detect(&c);
        let msg = out[0]["message"].as_str().unwrap().to_string();
        assert_eq!(msg, format!("Inbox #7 (contract-escalated): {}", "x".repeat(100)));
        // '' is falsy in JS → generic suggested_action even though the column is non-NULL
        assert_eq!(out[0]["suggested_action"], json!("open Inbox (i) to read the full body + suggested action"));
        assert_eq!(out[0]["detail"]["suggested_reconciler"], json!(""));
    }

    #[test]
    fn caps_at_10_newest() {
        let c = db();
        for i in 1..=12 {
            c.execute("INSERT INTO inbox_messages (id, ts, kind, subject, severity) VALUES (?1, '2025-12-30 16:00:00', 'driver-tick-failed', ?2, 'error')", rusqlite::params![i, format!("e{i}")]).unwrap();
        }
        let out = detect(&c);
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 10);
        assert_eq!(arr[0]["detail"]["inbox_id"], json!(12));
        assert_eq!(arr[9]["detail"]["inbox_id"], json!(3));
    }

    #[test]
    fn fix_is_manual_refusal_and_touches_nothing() {
        let c = db();
        let r = fix(&json!({ "detail": { "inbox_id": 1 } }), &c);
        assert_eq!(r, json!({ "applied": false, "reason": "manual — pick A (read) or B (archive)" }));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM inbox_messages", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0);
    }
}
