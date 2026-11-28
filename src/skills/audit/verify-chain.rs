//! verify-chain.rs — twin of skills/audit/verify-chain.mjs. NAMES ARE CONTRACT: `verifyChain`
//! (default, standard lane) · `walkSegment` (named, db-first helper). Re-hashes sacred_timeline
//! rows in id order to verify the audit Merkle chain is intact.
//!
//! `computeDigitalPrint` and `SEGMENT_BOUNDARY_OP` are inlined here rather than converting
//! skills/audit/_helpers/timeline-segments.mjs as a twin: that helper also does real filesystem
//! I/O (readdirSync/gzip/mkdtemp for segment archival) that is out of scope for this file. The JS
//! source's own comment on computeDigitalPrint says "Duplicated deliberately so the hot write
//! path (with-audit) keeps its own copy untouched" — with-audit.rs already duplicates the same
//! formula locally (see `compute_digital_print` there); this file follows the same precedent.
use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

// The operation of the bridge row that opens every post-rotation segment (timeline-segments.mjs).
const SEGMENT_BOUNDARY_OP: &str = "sacred_timeline.segment.boundary";

// computeDigitalPrint: sha256 hex of the 9 event fields joined by '|', in EXACT JS field order.
// `?? ''` fields (before_json/after_json/payload) pass an empty string when None/NULL.
#[allow(clippy::too_many_arguments)]
fn compute_digital_print(
    prev_print: &str,
    ts: &str,
    actor: &str,
    operation: &str,
    entity_type: &str,
    entity_id: &str,
    before_json: Option<&str>,
    after_json: Option<&str>,
    payload: Option<&str>,
) -> String {
    let content = [
        prev_print,
        ts,
        actor,
        operation,
        entity_type,
        entity_id,
        before_json.unwrap_or(""),
        after_json.unwrap_or(""),
        payload.unwrap_or(""),
    ]
    .join("|");
    hex::encode(Sha256::digest(content.as_bytes()))
}

// Walk one segment's rows in id order, threading the chain. `table_ref` is a (possibly
// schema-qualified) table name. `start_prev` is the prev_print the first row must match.
//
// FIX-VERIFY-CHAIN-OOM (2026-06-10): batch by id — the chain stores full entity snapshots, so
// .all()-ing thousands of rows at once blows memory. Peak memory = one batch; the walk is
// identical regardless of batch size.
pub fn walkSegment(db: &Connection, table_ref: &str, start_prev: &str, batch: i64) -> Value {
    let mut expected_prev = start_prev.to_string();
    let mut count: i64 = 0;
    let mut last_id: i64 = 0;
    let sql = format!("SELECT * FROM {table_ref} WHERE id > ? ORDER BY id ASC LIMIT ?");
    loop {
        let rows = match crate::db_json::query_rows(db, &sql, &[json!(last_id), json!(batch)]) {
            Ok(v) => v,
            Err(e) => return json!({ "ok": false, "error": e }),
        };
        let arr = rows.as_array().cloned().unwrap_or_default();
        if arr.is_empty() {
            break;
        }
        for row in &arr {
            let row_id = row.get("id").cloned().unwrap_or(Value::Null);
            let prev_print = row.get("prev_print").and_then(|v| v.as_str()).unwrap_or("");
            if prev_print != expected_prev {
                return json!({ "ok": false, "brokenAt": row_id, "reason": "prev_print mismatch" });
            }
            let recomputed = compute_digital_print(
                &expected_prev,
                row.get("ts").and_then(|v| v.as_str()).unwrap_or(""),
                row.get("actor").and_then(|v| v.as_str()).unwrap_or(""),
                row.get("operation").and_then(|v| v.as_str()).unwrap_or(""),
                row.get("entity_type").and_then(|v| v.as_str()).unwrap_or(""),
                row.get("entity_id").and_then(|v| v.as_str()).unwrap_or(""),
                row.get("before_json").and_then(|v| v.as_str()),
                row.get("after_json").and_then(|v| v.as_str()),
                row.get("payload").and_then(|v| v.as_str()),
            );
            let digital_print = row.get("digital_print").and_then(|v| v.as_str()).unwrap_or("");
            if recomputed != digital_print {
                return json!({ "ok": false, "brokenAt": row_id, "reason": "digital_print mismatch" });
            }
            expected_prev = digital_print.to_string();
            last_id = row.get("id").and_then(|v| v.as_i64()).unwrap_or(last_id);
            count += 1;
        }
    }
    json!({ "ok": true, "lastPrint": expected_prev, "count": count })
}

// JS: `if (!db) throw err.contextMissing('verify-chain', 'db')` — the db lane always injects a
// live db (a `&Connection`, never absent), so that guard is unreachable in this signature.
pub fn verifyChain(_input: &Value, db: &Connection) -> Result<Value, String> {
    // A genesis segment starts at GENESIS. A post-rotation LIVE segment instead opens with a
    // boundary row whose prev_print is the archived segment's last hash — accept that as the
    // anchor so the live timeline verifies standalone. (Confirming the anchor actually matches
    // the archived file is the job of verify-chain-full, which walks every segment as one chain.)
    let first = crate::db_json::query_rows(
        db,
        "SELECT prev_print, operation FROM sacred_timeline ORDER BY id ASC LIMIT 1",
        &[],
    )?;
    let first_row = first.as_array().and_then(|a| a.first());
    let mut start_prev = "GENESIS".to_string();
    if let Some(row) = first_row {
        let operation = row.get("operation").and_then(|v| v.as_str()).unwrap_or("");
        let prev_print = row.get("prev_print").and_then(|v| v.as_str()).unwrap_or("");
        if operation == SEGMENT_BOUNDARY_OP && prev_print != "GENESIS" {
            start_prev = prev_print.to_string();
        }
    }

    let r = walkSegment(db, "sacred_timeline", &start_prev, 500);
    let ok = r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if !ok {
        return Ok(json!({
            "valid": false,
            "brokenAt": r.get("brokenAt").cloned().unwrap_or(Value::Null),
            "reason": r.get("reason").cloned().unwrap_or(Value::Null),
        }));
    }
    let count = r.get("count").cloned().unwrap_or(json!(0));
    if start_prev == "GENESIS" {
        Ok(json!({ "valid": true, "count": count }))
    } else {
        Ok(json!({
            "valid": true,
            "count": count,
            "anchored": true,
            "anchorPrint": start_prev,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_db(sql: &str) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE sacred_timeline (
               id INTEGER PRIMARY KEY, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT,
               entity_id TEXT, before_json TEXT, after_json TEXT, payload TEXT,
               prev_print TEXT, digital_print TEXT
             );",
        )
        .unwrap();
        if !sql.is_empty() {
            c.execute_batch(sql).unwrap();
        }
        c
    }

    #[test]
    fn empty_timeline_is_valid() {
        let c = seeded_db("");
        let v = verifyChain(&Value::Null, &c).unwrap();
        assert_eq!(v, json!({ "valid": true, "count": 0 }));
    }

    #[test]
    fn intact_chain_is_valid() {
        let c = seeded_db(
            "INSERT INTO sacred_timeline (id, ts, actor, operation, entity_type, entity_id, before_json, after_json, payload, prev_print, digital_print) VALUES \
             (1, '2026-01-01T00:00:00.000Z', 't', 'card.add', 'card', 'A', NULL, '{\"slug\":\"A\",\"title\":\"a\"}', NULL, 'GENESIS', 'e488e6fd7a2ba805f9827ef6b66e767597b207b0359c6231119478be419e6754'), \
             (2, '2026-01-01T00:05:00.000Z', 't', 'card.add', 'card', 'B', NULL, '{\"slug\":\"B\",\"title\":\"b\"}', NULL, 'e488e6fd7a2ba805f9827ef6b66e767597b207b0359c6231119478be419e6754', 'a914ade38681bb347a79baa60704e2c823c1579f448e8fccdf86712bf1e06644');",
        );
        let v = verifyChain(&Value::Null, &c).unwrap();
        assert_eq!(v["valid"], json!(true));
        assert_eq!(v["count"], json!(2));
    }

    #[test]
    fn tampered_actor_breaks_digital_print() {
        let c = seeded_db(
            "INSERT INTO sacred_timeline (id, ts, actor, operation, entity_type, entity_id, before_json, after_json, payload, prev_print, digital_print) VALUES \
             (1, '2026-01-01T00:00:00.000Z', 'attacker', 'card.add', 'card', 'A', NULL, '{\"slug\":\"A\",\"title\":\"a\"}', NULL, 'GENESIS', 'e488e6fd7a2ba805f9827ef6b66e767597b207b0359c6231119478be419e6754');",
        );
        let v = verifyChain(&Value::Null, &c).unwrap();
        assert_eq!(v["valid"], json!(false));
        assert_eq!(v["brokenAt"], json!(1));
        assert_eq!(v["reason"], json!("digital_print mismatch"));
    }

    #[test]
    fn broken_prev_print_link() {
        let c = seeded_db(
            "INSERT INTO sacred_timeline (id, ts, actor, operation, entity_type, entity_id, before_json, after_json, payload, prev_print, digital_print) VALUES \
             (1, '2026-01-01T00:00:00.000Z', 't', 'card.add', 'card', 'A', NULL, '{\"slug\":\"A\",\"title\":\"a\"}', NULL, 'GENESIS', 'e488e6fd7a2ba805f9827ef6b66e767597b207b0359c6231119478be419e6754'), \
             (2, '2026-01-01T00:05:00.000Z', 't', 'card.add', 'card', 'B', NULL, '{\"slug\":\"B\",\"title\":\"b\"}', NULL, 'WRONG', 'a914ade38681bb347a79baa60704e2c823c1579f448e8fccdf86712bf1e06644');",
        );
        let v = verifyChain(&Value::Null, &c).unwrap();
        assert_eq!(v["valid"], json!(false));
        assert_eq!(v["brokenAt"], json!(2));
        assert_eq!(v["reason"], json!("prev_print mismatch"));
    }

    #[test]
    fn anchored_boundary_segment_is_valid() {
        let anchor = "a".repeat(64);
        let c = seeded_db(&format!(
            "INSERT INTO sacred_timeline (id, ts, actor, operation, entity_type, entity_id, before_json, after_json, payload, prev_print, digital_print) VALUES \
             (1, '2026-01-01T00:10:00.000Z', 'system', 'sacred_timeline.segment.boundary', 'timeline', 'seg-2', NULL, NULL, '{{\"rotated\":true}}', '{anchor}', '448c9c01d719fde2a69aa8fd728565f03a2baaec9107ab87b779e050f0a6311f');"
        ));
        let v = verifyChain(&Value::Null, &c).unwrap();
        assert_eq!(v["valid"], json!(true));
        assert_eq!(v["count"], json!(1));
        assert_eq!(v["anchored"], json!(true));
        assert_eq!(v["anchorPrint"], json!(anchor));
    }

    #[test]
    fn walk_segment_paginates_in_small_batches() {
        let c = seeded_db(
            "INSERT INTO sacred_timeline (id, ts, actor, operation, entity_type, entity_id, before_json, after_json, payload, prev_print, digital_print) VALUES \
             (1, '2026-01-01T00:00:00.000Z', 't', 'card.add', 'card', 'A', NULL, '{\"slug\":\"A\",\"title\":\"a\"}', NULL, 'GENESIS', 'e488e6fd7a2ba805f9827ef6b66e767597b207b0359c6231119478be419e6754'), \
             (2, '2026-01-01T00:05:00.000Z', 't', 'card.add', 'card', 'B', NULL, '{\"slug\":\"B\",\"title\":\"b\"}', NULL, 'e488e6fd7a2ba805f9827ef6b66e767597b207b0359c6231119478be419e6754', 'a914ade38681bb347a79baa60704e2c823c1579f448e8fccdf86712bf1e06644');",
        );
        let r = walkSegment(&c, "sacred_timeline", "GENESIS", 1);
        assert_eq!(
            r,
            json!({ "ok": true, "lastPrint": "a914ade38681bb347a79baa60704e2c823c1579f448e8fccdf86712bf1e06644", "count": 2 })
        );
    }
}
