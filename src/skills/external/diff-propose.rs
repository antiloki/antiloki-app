//! diff-propose.rs — twin of skills/external/diff-propose.mjs (db WRITE lane). NAMES ARE CONTRACT:
//! `diffPropose` (default). Files a pending_diffs proposal; supersedes any prior pending for the
//! same (card, file_path). datetime('now') is pinned to the frozen instant (2026-01-01 00:00:00).
// QUALITY: 82/100 — required-field + path guards, card-existence check, supersede count, id read-back
use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => v.to_string(),
    }
}

pub fn diffPropose(input: &Value, db: &Connection) -> Result<Value, String> {
    let card_slug = input.get("card_slug").cloned().unwrap_or(Value::Null);
    let file_path = input.get("file_path").cloned().unwrap_or(Value::Null);
    // JS destructuring default: applies only when the key is absent (undefined), not on null.
    let proposed_by = match input.get("proposed_by") {
        None => json!("ai:unknown"),
        Some(v) => v.clone(),
    };

    if !truthy(Some(&card_slug)) {
        return Err("diff-propose: card_slug is required".into());
    }
    if !truthy(Some(&file_path)) {
        return Err("diff-propose: file_path is required".into());
    }
    // !diff_text || typeof diff_text !== 'string' → must be a non-empty string.
    let diff_text = match input.get("diff_text") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Err("diff-propose: diff_text is required".into()),
    };

    // VALID_FILE_PATH = /^[^\\\0]+$/ — non-empty, no backslash, no NUL.
    let fp = file_path.as_str().unwrap_or("");
    if fp.is_empty() || fp.contains('\\') || fp.contains('\0') {
        return Err(format!(
            "diff-propose: file_path contains invalid characters: '{}'",
            js_string(&file_path)
        ));
    }
    if fp.contains("..") {
        return Err("diff-propose: file_path may not contain parent-traversal (..)".into());
    }

    // Verify the card exists (no orphan proposals).
    let card = crate::db_json::query_rows(db, "SELECT slug FROM cards WHERE slug = ?", &[card_slug.clone()])?;
    if card.as_array().map_or(true, |a| a.is_empty()) {
        return Err(format!(
            "diff-propose: card '{}' not found; cannot propose diff for unknown card",
            js_string(&card_slug)
        ));
    }

    // Supersede any prior pending proposal for the same (card, file_path).
    // datetime('now') is pinned to the frozen instant so both lanes match.
    let (superseded, _) = crate::db_json::run_write(
        db,
        &format!("UPDATE pending_diffs \
         SET status = 'superseded', decided_at = {}, \
             decided_by = 'system:auto-supersede', \
             decision_reason = 'newer proposal arrived' \
         WHERE card_slug = ? AND file_path = ? AND status = 'pending'", crate::clock::now_sql()),
        &[card_slug.clone(), file_path.clone()],
    )?;

    let (_c, id) = crate::db_json::run_write(
        db,
        "INSERT INTO pending_diffs (card_slug, file_path, diff_text, proposed_by) VALUES (?, ?, ?, ?)",
        &[card_slug.clone(), file_path.clone(), json!(diff_text), proposed_by.clone()],
    )?;

    // diff_text.length — UTF-16 code units; chars().count() matches for BMP (fixtures stay ASCII).
    let diff_bytes = diff_text.chars().count() as i64;

    Ok(json!({
        "id": id,
        "card_slug": card_slug,
        "file_path": file_path,
        "proposed_by": proposed_by,
        "status": "pending",
        "superseded_count": superseded as i64,
        "diff_bytes": diff_bytes,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY); \
             CREATE TABLE pending_diffs ( id INTEGER PRIMARY KEY, card_slug TEXT NOT NULL, \
               file_path TEXT NOT NULL, diff_text TEXT NOT NULL, proposed_by TEXT NOT NULL, \
               status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','approved','rejected','applied','superseded')), \
               proposed_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', decided_at TEXT, decided_by TEXT, \
               decision_reason TEXT, applied_at TEXT, applied_sha TEXT );",
        )
        .unwrap();
        c
    }

    #[test]
    fn inserts_and_supersedes() {
        let c = fresh();
        c.execute("INSERT INTO cards (slug) VALUES ('PROP-1')", []).unwrap();

        // fresh proposal → superseded_count 0, id 1, correct diff_bytes
        let r = diffPropose(
            &json!({ "card_slug": "PROP-1", "file_path": "a.mjs", "diff_text": "hello", "proposed_by": "ai:openai" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["id"], json!(1));
        assert_eq!(r["status"], json!("pending"));
        assert_eq!(r["superseded_count"], json!(0));
        assert_eq!(r["proposed_by"], json!("ai:openai"));
        assert_eq!(r["diff_bytes"], json!(5));

        // second proposal, same (card,file) → supersedes the first; default proposed_by
        let r2 = diffPropose(
            &json!({ "card_slug": "PROP-1", "file_path": "a.mjs", "diff_text": "world!!" }),
            &c,
        )
        .unwrap();
        assert_eq!(r2["superseded_count"], json!(1));
        assert_eq!(r2["proposed_by"], json!("ai:unknown"));
        assert_eq!(r2["id"], json!(2));

        // the superseded row carries the frozen decided_at
        let da: String = c.query_row("SELECT decided_at FROM pending_diffs WHERE id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(da, "2026-01-01 00:00:00");
        let st: String = c.query_row("SELECT status FROM pending_diffs WHERE id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(st, "superseded");
    }

    #[test]
    fn different_file_does_not_supersede() {
        let c = fresh();
        c.execute("INSERT INTO cards (slug) VALUES ('PROP-M')", []).unwrap();
        diffPropose(&json!({ "card_slug": "PROP-M", "file_path": "a.mjs", "diff_text": "d1" }), &c).unwrap();
        let r = diffPropose(&json!({ "card_slug": "PROP-M", "file_path": "b.mjs", "diff_text": "d2" }), &c).unwrap();
        assert_eq!(r["superseded_count"], json!(0));
    }

    #[test]
    fn guards() {
        let c = fresh();
        assert!(diffPropose(&json!({}), &c).unwrap_err().contains("card_slug is required"));
        assert!(diffPropose(&json!({ "card_slug": "X" }), &c).unwrap_err().contains("file_path is required"));
        assert!(diffPropose(&json!({ "card_slug": "X", "file_path": "a.mjs" }), &c).unwrap_err().contains("diff_text is required"));
        assert!(diffPropose(&json!({ "card_slug": "X", "file_path": "../x", "diff_text": "d" }), &c).unwrap_err().contains("parent-traversal"));
        assert!(diffPropose(&json!({ "card_slug": "X", "file_path": "a\\b", "diff_text": "d" }), &c).unwrap_err().contains("invalid characters"));
        assert!(diffPropose(&json!({ "card_slug": "NOPE", "file_path": "a.mjs", "diff_text": "d" }), &c).unwrap_err().contains("not found"));
    }
}
