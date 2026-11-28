//! contract-admin-reject.rs — twin of skills/contract/contract-admin-reject.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `contractAdminReject` (default). God-gated admin override: flip a contract
//! to status='rejected' bypassing the tier check. SELECT slug/status/tier, guard, then UPDATE.
// QUALITY: — validation (slug / reason≥50 chars / failure_kind) + not-found + already-rejected
// guards; UPDATE stamps updated_at=datetime('now') frozen to 2026-01-01. Success-path fixtured.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS truthiness for the `!x` guards (undefined/null/empty-string/0/false → falsy).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

pub fn contractAdminReject(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = input.get("slug");
    if !truthy(slug) {
        return Err("contract-admin-reject: slug is required".into());
    }
    let reason = input.get("reason");
    // JS: !reason || reason.trim().length < 50
    let reason_trim_len = reason.and_then(|v| v.as_str()).unwrap_or("").trim().chars().count();
    if !truthy(reason) || reason_trim_len < 50 {
        return Err("contract-admin-reject: reason must be at least 50 chars (this is an admin override; document why)".into());
    }
    let failure_kind = input.get("failure_kind");
    if !truthy(failure_kind) {
        return Err("contract-admin-reject: failure_kind is required".into());
    }
    let slug_v = slug.cloned().unwrap();

    let existing = crate::db_json::query_rows(db, "SELECT slug, status, tier FROM contracts WHERE slug = ?", &[slug_v.clone()])?;
    let existing = existing.as_array().unwrap();
    let row = match existing.first() {
        Some(r) => r,
        None => return Err(format!("contract-admin-reject: contract '{}' not found", slug_v.as_str().unwrap_or_default())),
    };
    let prev_status = row.get("status").cloned().unwrap_or(Value::Null);
    if prev_status.as_str() == Some("rejected") {
        return Err(format!("contract-admin-reject: contract '{}' is already rejected — no-op refused", slug_v.as_str().unwrap_or_default()));
    }

    // JS: UPDATE ... updated_at=datetime('now') — frozen-clock lane pins 'now' to the instant below.
    crate::db_json::run_write(
        db,
        &format!("UPDATE contracts SET status='rejected', rejection_reason_md=?, failure_kind=?, updated_at={} WHERE slug = ?", crate::clock::now_sql()),
        &[reason.cloned().unwrap(), failure_kind.cloned().unwrap(), slug_v.clone()],
    )?;

    // Return object — insertion order mirrors the JS object literal exactly.
    let mut out = Map::new();
    out.insert("slug".into(), slug_v);
    out.insert("prev_status".into(), prev_status);
    out.insert("new_status".into(), json!("rejected"));
    out.insert("failure_kind".into(), failure_kind.cloned().unwrap());
    out.insert("reason".into(), reason.cloned().unwrap());
    out.insert("tier_was".into(), row.get("tier").cloned().unwrap_or(Value::Null));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    const REASON: &str = "CONTRACT-X is wedged: drafted pre-fix with poisoned files_touched. Re-routing via auto-redraft so the new code path produces a clean replacement.";
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE contracts (slug TEXT PRIMARY KEY, status TEXT, tier TEXT, rejection_reason_md TEXT, failure_kind TEXT, updated_at TEXT); \
            INSERT INTO contracts (slug,status,tier,updated_at) VALUES ('C-STD','tl_approved','standard','2025-12-01 00:00:00');").unwrap();
        c
    }
    #[test]
    fn rejects_and_stamps() {
        let c = seeded();
        let r = contractAdminReject(&json!({ "slug": "C-STD", "reason": REASON, "failure_kind": "poisoned_files_touched" }), &c).unwrap();
        assert_eq!(r["slug"], json!("C-STD"));
        assert_eq!(r["prev_status"], json!("tl_approved"));
        assert_eq!(r["new_status"], json!("rejected"));
        assert_eq!(r["failure_kind"], json!("poisoned_files_touched"));
        assert_eq!(r["reason"], json!(REASON));
        assert_eq!(r["tier_was"], json!("standard"));
        // key order matches the JS object literal
        assert_eq!(r.as_object().unwrap().keys().cloned().collect::<Vec<_>>(),
            vec!["slug", "prev_status", "new_status", "failure_kind", "reason", "tier_was"]);
        // db mutated: status/reason/failure_kind + frozen updated_at
        let after = crate::db_json::query_rows(&c, "SELECT status, rejection_reason_md, failure_kind, updated_at FROM contracts WHERE slug = ?", &[json!("C-STD")]).unwrap();
        let row = after.as_array().unwrap().first().unwrap();
        assert_eq!(row["status"], json!("rejected"));
        assert_eq!(row["failure_kind"], json!("poisoned_files_touched"));
        assert_eq!(row["rejection_reason_md"], json!(REASON));
        assert_eq!(row["updated_at"], json!("2026-01-01 00:00:00"));
    }
    #[test]
    fn guards() {
        let c = seeded();
        assert!(contractAdminReject(&json!({ "reason": REASON, "failure_kind": "fk" }), &c).is_err()); // missing slug
        assert!(contractAdminReject(&json!({ "slug": "C-STD", "reason": "too short", "failure_kind": "fk" }), &c).is_err()); // reason < 50
        assert!(contractAdminReject(&json!({ "slug": "C-STD", "reason": REASON }), &c).is_err()); // missing failure_kind
        assert!(contractAdminReject(&json!({ "slug": "NOPE", "reason": REASON, "failure_kind": "fk" }), &c).is_err()); // not found
        // already rejected → no-op refused
        c.execute_batch("UPDATE contracts SET status='rejected' WHERE slug='C-STD';").unwrap();
        assert!(contractAdminReject(&json!({ "slug": "C-STD", "reason": REASON, "failure_kind": "fk" }), &c).is_err());
    }
}
