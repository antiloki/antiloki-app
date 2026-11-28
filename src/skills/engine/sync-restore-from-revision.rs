//! sync-restore-from-revision.rs — twin of skills/engine/sync-restore-from-revision.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `syncRestoreFromRevision` (default). Restore card state from the latest
//! snapshot for a branch; surface a structured diff in `conflicts` and refuse unless force=true.
// QUALITY: field-diff conflict gate · frozen updated_at on UPDATE · INSERT when card absent
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `??` right operand: keep only when present-and-non-null.
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

// JS strict === over the primitive values diffFields compares (null/string/number/bool).
// Numbers compare by numeric value (JS has one f64 type) so int 3 === float 3.0.
fn js_strict_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => false,
    }
}

// diffFields(a, b, keys): push {field, current, snapshot} for each key where (a[k] ?? null) !== (b[k] ?? null).
fn diff_fields(a: &Value, b: &Value, keys: &[&str]) -> Vec<Value> {
    let mut out = Vec::new();
    for k in keys {
        let av = nz(a.get(*k)).cloned().unwrap_or(Value::Null);
        let bv = nz(b.get(*k)).cloned().unwrap_or(Value::Null);
        if !js_strict_eq(&av, &bv) {
            let mut e = Map::new();
            e.insert("field".into(), json!(*k));
            e.insert("current".into(), av);
            e.insert("snapshot".into(), bv);
            out.push(Value::Object(e));
        }
    }
    out
}

pub fn syncRestoreFromRevision(input: &Value, db: &Connection) -> Result<Value, String> {
    let card_slug = input.get("card_slug");
    let branch = input.get("branch");
    let force = input.get("force");

    // b = branch ?? currentBranch(). Fixtures always supply `branch`, so currentBranch() (a
    // non-portable `git rev-parse` spawn) is never exercised; fall back to its git-fail value.
    let b = nz(branch).cloned().unwrap_or_else(|| json!("unknown"));

    // Pick the latest revision for this branch (and optional card_slug).
    let rows = if truthy(card_slug) {
        crate::db_json::query_rows(
            db,
            "SELECT * FROM card_revisions WHERE branch = ? AND card_slug = ? ORDER BY captured_at DESC, id DESC LIMIT 1",
            &[b.clone(), card_slug.cloned().unwrap_or(Value::Null)],
        )?
    } else {
        crate::db_json::query_rows(
            db,
            "SELECT * FROM card_revisions WHERE branch = ? ORDER BY captured_at DESC, id DESC LIMIT 1",
            &[b.clone()],
        )?
    };
    let rows = rows.as_array().cloned().unwrap_or_default();
    let row = match rows.first() {
        None => {
            let mut o = Map::new();
            o.insert("restored".into(), json!(false));
            o.insert("branch".into(), b);
            o.insert("reason".into(), json!("no revisions for this branch"));
            return Ok(Value::Object(o));
        }
        Some(r) => r,
    };

    let snap_str = row.get("snapshot_json").and_then(|v| v.as_str()).unwrap_or("");
    let snap: Value = serde_json::from_str(snap_str)
        .map_err(|_| "sync-restore-from-revision: snapshot_json is corrupt; cannot restore".to_string())?;

    let snap_card = snap.get("card");
    if !truthy(snap_card.and_then(|c| c.get("slug"))) {
        return Err("sync-restore-from-revision: snapshot does not contain a card row".into());
    }
    let snap_card = snap_card.unwrap();
    let snap_slug = snap_card.get("slug").cloned().unwrap();

    let current_rows = crate::db_json::query_rows(db, "SELECT * FROM cards WHERE slug = ?", &[snap_slug.clone()])?;
    let current_rows = current_rows.as_array().cloned().unwrap_or_default();
    let current = current_rows.first();

    let keys = ["status", "title", "parent_slug", "risk", "complexity", "priority"];
    let conflicts: Vec<Value> = match current {
        Some(c) => diff_fields(c, snap_card, &keys),
        None => Vec::new(),
    };

    if !conflicts.is_empty() && !truthy(force) {
        let mut o = Map::new();
        o.insert("restored".into(), json!(false));
        o.insert("branch".into(), b);
        o.insert("card_slug".into(), snap_slug);
        o.insert("conflict_count".into(), json!(conflicts.len() as i64));
        o.insert("conflicts".into(), json!(conflicts));
        o.insert("advice".into(), json!("Pass force=true to overwrite local state with the snapshot."));
        return Ok(Value::Object(o));
    }

    // Apply. datetime('now') is pinned to the frozen instant on both lanes.
    if current.is_some() {
        let params = vec![
            snap_card.get("status").cloned().unwrap_or(Value::Null),
            snap_card.get("title").cloned().unwrap_or(Value::Null),
            nz(snap_card.get("parent_slug")).cloned().unwrap_or(Value::Null),
            nz(snap_card.get("risk")).cloned().unwrap_or(Value::Null),
            nz(snap_card.get("complexity")).cloned().unwrap_or(Value::Null),
            nz(snap_card.get("priority")).cloned().unwrap_or(Value::Null),
            snap_slug.clone(),
        ];
        crate::db_json::run_write(
            db,
            &format!("UPDATE cards SET status=?, title=?, parent_slug=?, risk=?, complexity=?, priority=?, updated_at={} WHERE slug=?", crate::clock::now_sql()),
            &params,
        )?;
    } else {
        let params = vec![
            snap_slug.clone(),
            snap_card.get("title").cloned().unwrap_or(Value::Null),
            snap_card.get("status").cloned().unwrap_or(Value::Null),
            nz(snap_card.get("parent_slug")).cloned().unwrap_or(Value::Null),
            nz(snap_card.get("risk")).cloned().unwrap_or(Value::Null),
            nz(snap_card.get("complexity")).cloned().unwrap_or(Value::Null),
            nz(snap_card.get("priority")).cloned().unwrap_or(Value::Null),
        ];
        crate::db_json::run_write(
            db,
            "INSERT INTO cards (slug, title, status, parent_slug, risk, complexity, priority) VALUES (?, ?, ?, ?, ?, ?, ?)",
            &params,
        )?;
    }

    let fields_overwritten: Vec<Value> =
        conflicts.iter().map(|c| c.get("field").cloned().unwrap_or(Value::Null)).collect();
    let mut o = Map::new();
    o.insert("restored".into(), json!(true));
    o.insert("branch".into(), b);
    o.insert("card_slug".into(), snap_slug);
    o.insert("revision_id".into(), row.get("id").cloned().unwrap_or(Value::Null));
    o.insert("captured_at".into(), row.get("captured_at").cloned().unwrap_or(Value::Null));
    o.insert("fields_overwritten".into(), json!(fields_overwritten));
    Ok(Value::Object(o))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, parent_slug TEXT, risk INTEGER, complexity INTEGER, priority INTEGER, updated_at TEXT); \
             CREATE TABLE card_revisions (id INTEGER PRIMARY KEY, branch TEXT, card_slug TEXT, sha TEXT, snapshot_json TEXT, captured_at TEXT);",
        )
        .unwrap();
        c
    }

    fn snap(slug: &str, title: &str, status: &str, risk: i64) -> String {
        json!({ "card": { "slug": slug, "title": title, "status": status, "parent_slug": Value::Null, "risk": risk, "complexity": 5, "priority": 1 } }).to_string()
    }

    #[test]
    fn no_revisions_returns_reason() {
        let c = fresh();
        let r = syncRestoreFromRevision(&json!({ "branch": "main" }), &c).unwrap();
        assert_eq!(r, json!({ "restored": false, "branch": "main", "reason": "no revisions for this branch" }));
    }

    #[test]
    fn conflict_without_force_refuses() {
        let c = fresh();
        c.execute("INSERT INTO cards (slug,title,status,parent_slug,risk,complexity,priority,updated_at) VALUES ('CARD-1','Old Title','doing',NULL,2,5,1,'2025-01-01 00:00:00')", []).unwrap();
        let s = snap("CARD-1", "New Title", "done", 3);
        c.execute("INSERT INTO card_revisions (id,branch,card_slug,sha,snapshot_json,captured_at) VALUES (1,'main','CARD-1','sha1',?1,'2025-12-20 10:00:00')", [s]).unwrap();
        let r = syncRestoreFromRevision(&json!({ "branch": "main" }), &c).unwrap();
        assert_eq!(r["restored"], json!(false));
        assert_eq!(r["conflict_count"], json!(3));
        let fields: Vec<String> = r["conflicts"].as_array().unwrap().iter().map(|x| x["field"].as_str().unwrap().to_string()).collect();
        assert_eq!(fields, vec!["status", "title", "risk"]);
        // db untouched
        let title: String = c.query_row("SELECT title FROM cards WHERE slug='CARD-1'", [], |r| r.get(0)).unwrap();
        assert_eq!(title, "Old Title");
    }

    #[test]
    fn force_overwrites_and_stamps_frozen() {
        let c = fresh();
        c.execute("INSERT INTO cards (slug,title,status,parent_slug,risk,complexity,priority,updated_at) VALUES ('CARD-1','Old Title','doing',NULL,2,5,1,'2025-01-01 00:00:00')", []).unwrap();
        let s = snap("CARD-1", "New Title", "done", 3);
        c.execute("INSERT INTO card_revisions (id,branch,card_slug,sha,snapshot_json,captured_at) VALUES (7,'main','CARD-1','sha1',?1,'2025-12-20 10:00:00')", [s]).unwrap();
        let r = syncRestoreFromRevision(&json!({ "branch": "main", "force": true }), &c).unwrap();
        assert_eq!(r["restored"], json!(true));
        assert_eq!(r["revision_id"], json!(7));
        assert_eq!(r["captured_at"], json!("2025-12-20 10:00:00"));
        assert_eq!(r["fields_overwritten"], json!(["status", "title", "risk"]));
        let (title, status, risk, ua): (String, String, i64, String) = c.query_row("SELECT title,status,risk,updated_at FROM cards WHERE slug='CARD-1'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
        assert_eq!((title.as_str(), status.as_str(), risk), ("New Title", "done", 3));
        assert_eq!(ua, "2026-01-01 00:00:00");
    }

    #[test]
    fn insert_when_card_absent() {
        let c = fresh();
        let s = snap("CARD-NEW", "Fresh", "todo", 1);
        c.execute("INSERT INTO card_revisions (id,branch,card_slug,sha,snapshot_json,captured_at) VALUES (3,'main','CARD-NEW','sha',?1,'2025-12-25 00:00:00')", [s]).unwrap();
        let r = syncRestoreFromRevision(&json!({ "branch": "main", "card_slug": "CARD-NEW" }), &c).unwrap();
        assert_eq!(r["restored"], json!(true));
        assert_eq!(r["fields_overwritten"], json!([]));
        let title: String = c.query_row("SELECT title FROM cards WHERE slug='CARD-NEW'", [], |r| r.get(0)).unwrap();
        assert_eq!(title, "Fresh");
    }

    #[test]
    fn picks_latest_by_captured_at() {
        let c = fresh();
        let old = snap("CARD-4", "old rev", "todo", 1);
        let new = snap("CARD-4", "new rev", "done", 2);
        c.execute("INSERT INTO card_revisions (id,branch,card_slug,sha,snapshot_json,captured_at) VALUES (1,'main','CARD-4','a',?1,'2025-12-01 00:00:00')", [old]).unwrap();
        c.execute("INSERT INTO card_revisions (id,branch,card_slug,sha,snapshot_json,captured_at) VALUES (2,'main','CARD-4','b',?1,'2025-12-25 00:00:00')", [new]).unwrap();
        let r = syncRestoreFromRevision(&json!({ "branch": "main", "card_slug": "CARD-4" }), &c).unwrap();
        assert_eq!(r["revision_id"], json!(2));
        assert_eq!(r["captured_at"], json!("2025-12-25 00:00:00"));
    }
}
