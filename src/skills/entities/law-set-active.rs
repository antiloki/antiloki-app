//! law-set-active.rs — twin of skills/entities/law-set-active.mjs (db WRITE lane, cross-db laws
//! via ATTACH). NAMES ARE CONTRACT: `lawSetActive` (default) · `isCustomLaw` (named helper).
//! "Active" rides the enforcement column: activate → 'soft', deactivate → 'archived'. Only
//! CUSTOM laws (non-hard, non-protected kind, established_by starting 'interviewer') may toggle.
use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }

/// JS: soft-or-archived enforcement, kind outside PROTECTED_KINDS {constitutional, constraint},
/// AND established_by is a string starting with 'interviewer'. Everything else is sacred.
pub fn isCustomLaw(row: &Value) -> Value {
    // JS `row.enforcement !== 'hard'` — any non-'hard' value (null/absent/number) passes.
    let not_hard = row.get("enforcement").and_then(|v| v.as_str()) != Some("hard");
    // Set.has(kind) with a non-string kind is false in JS → only the two string kinds protect.
    let protected_kind = matches!(row.get("kind").and_then(|v| v.as_str()), Some("constitutional") | Some("constraint"));
    let interviewer = row.get("established_by").and_then(|v| v.as_str()).map_or(false, |s| s.starts_with("interviewer"));
    json!(not_hard && !protected_kind && interviewer)
}

pub fn lawSetActive(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}` — the db lane always supplies db, so the
    // contextMissing guard cannot fire here.
    let slug_v = input.get("slug");
    if !truthy(slug_v) { return Err("law-set-active: slug is required".into()); }
    let slug = slug_v.expect("truthy implies present").clone();
    let active = match input.get("active") {
        Some(Value::Bool(b)) => *b,
        _ => return Err("law-set-active: active (boolean) is required".into()),
    };

    let rows = crate::db_json::query_rows(db,
        "SELECT * FROM laws.laws WHERE slug = ? AND superseded_by IS NULL", &[slug.clone()])?;
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("law-set-active: law slug '{}' not found", tmpl(Some(&slug)))),
    };

    if isCustomLaw(&row) != json!(true) {
        return Err(format!("law-set-active: law '{}' is a protected (sacred) law \u{2014} only custom operator-added laws can be activated/deactivated", tmpl(Some(&slug))));
    }

    let target = if active { "soft" } else { "archived" };
    // JS `row.enforcement !== target` — string-vs-string strict compare; NULL/absent → UPDATE.
    if row.get("enforcement").and_then(|v| v.as_str()) != Some(target) {
        crate::db_json::run_write(db, "UPDATE laws.laws SET enforcement = ? WHERE id = ?",
            &[json!(target), row.get("id").cloned().unwrap_or(Value::Null)])?;
    }
    let out = crate::db_json::query_rows(db, "SELECT * FROM laws.laws WHERE id = ?",
        &[row.get("id").cloned().unwrap_or(Value::Null)])?;
    // JS `.get()` → the row object (or undefined → null through the lane).
    Ok(out.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; \
          CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT NOT NULL, title TEXT, kind TEXT NOT NULL, enforcement TEXT NOT NULL, established_by TEXT, superseded_by TEXT); \
          INSERT INTO laws.laws (id,slug,title,kind,enforcement,established_by,superseded_by) VALUES \
            (1,'CUSTOM-NO-FRIDAY-DEPLOYS','No deploys on Friday','procedural','soft','interviewer:fe',NULL), \
            (2,'NEVER_COMMIT_SECRETS','Never commit secrets','constitutional','hard','system:bootstrap',NULL), \
            (3,'CUSTOM-QUIET-HOURS','Quiet hours','decision','archived','interviewer',NULL);").unwrap();
        c
    }

    #[test]
    fn is_custom_law_truth_table() {
        assert_eq!(isCustomLaw(&json!({ "enforcement": "soft", "kind": "procedural", "established_by": "interviewer:fe" })), json!(true));
        assert_eq!(isCustomLaw(&json!({ "enforcement": "archived", "kind": "decision", "established_by": "interviewer" })), json!(true));
        assert_eq!(isCustomLaw(&json!({ "enforcement": "hard", "kind": "procedural", "established_by": "interviewer:fe" })), json!(false)); // hard
        assert_eq!(isCustomLaw(&json!({ "enforcement": "soft", "kind": "constitutional", "established_by": "interviewer:fe" })), json!(false)); // protected kind
        assert_eq!(isCustomLaw(&json!({ "enforcement": "soft", "kind": "constraint", "established_by": "interviewer:fe" })), json!(false)); // protected kind
        assert_eq!(isCustomLaw(&json!({ "enforcement": "soft", "kind": "procedural", "established_by": "human:dani" })), json!(false)); // not interviewer
        assert_eq!(isCustomLaw(&json!({ "enforcement": "soft", "kind": "procedural", "established_by": null })), json!(false)); // typeof null !== 'string'
        assert_eq!(isCustomLaw(&json!({ "enforcement": "soft", "kind": "procedural" })), json!(false)); // absent established_by
        assert_eq!(isCustomLaw(&json!({ "enforcement": "soft", "kind": "procedural", "established_by": 7 })), json!(false)); // non-string
        assert_eq!(isCustomLaw(&json!({ "established_by": "interviewer:fe" })), json!(true)); // absent enforcement+kind both pass
    }

    #[test]
    fn deactivate_flips_soft_to_archived() {
        let c = db();
        let row = lawSetActive(&json!({ "slug": "CUSTOM-NO-FRIDAY-DEPLOYS", "active": false }), &c).unwrap();
        assert_eq!(row["enforcement"], json!("archived"));
        assert_eq!(row["id"], json!(1));
        // full-row shape follows the schema column order (SELECT *)
        assert_eq!(serde_json::to_string(&row).unwrap(),
            "{\"id\":1,\"slug\":\"CUSTOM-NO-FRIDAY-DEPLOYS\",\"title\":\"No deploys on Friday\",\"kind\":\"procedural\",\"enforcement\":\"archived\",\"established_by\":\"interviewer:fe\",\"superseded_by\":null}");
    }

    #[test]
    fn reactivate_restores_soft_and_noop_paths() {
        let c = db();
        let row = lawSetActive(&json!({ "slug": "CUSTOM-QUIET-HOURS", "active": true }), &c).unwrap();
        assert_eq!(row["enforcement"], json!("soft"));
        // no-op: already soft, activate again — same row back, no write needed
        let again = lawSetActive(&json!({ "slug": "CUSTOM-QUIET-HOURS", "active": true }), &c).unwrap();
        assert_eq!(again["enforcement"], json!("soft"));
        // no-op deactivate on an archived law
        lawSetActive(&json!({ "slug": "CUSTOM-NO-FRIDAY-DEPLOYS", "active": false }), &c).unwrap();
        let still = lawSetActive(&json!({ "slug": "CUSTOM-NO-FRIDAY-DEPLOYS", "active": false }), &c).unwrap();
        assert_eq!(still["enforcement"], json!("archived"));
    }

    #[test]
    fn superseded_rows_are_skipped() {
        let c = db();
        c.execute_batch("INSERT INTO laws.laws (id,slug,title,kind,enforcement,established_by,superseded_by) VALUES \
            (4,'CUSTOM-X','Old v1','procedural','soft','interviewer:fe','CUSTOM-X-V2'), \
            (5,'CUSTOM-X','Current v2','procedural','archived','interviewer:fe',NULL);").unwrap();
        let row = lawSetActive(&json!({ "slug": "CUSTOM-X", "active": true }), &c).unwrap();
        assert_eq!(row["id"], json!(5)); // the non-superseded row was picked and flipped
        assert_eq!(row["enforcement"], json!("soft"));
    }

    #[test]
    fn guards_reject_invalid_input() {
        let c = db();
        assert!(lawSetActive(&json!({ "active": true }), &c).is_err()); // slug required
        assert!(lawSetActive(&json!({ "slug": "CUSTOM-QUIET-HOURS" }), &c).is_err()); // active missing
        assert!(lawSetActive(&json!({ "slug": "CUSTOM-QUIET-HOURS", "active": "yes" }), &c).is_err()); // non-boolean
        assert!(lawSetActive(&json!({ "slug": "GHOST", "active": true }), &c).is_err()); // not found
        let sacred = lawSetActive(&json!({ "slug": "NEVER_COMMIT_SECRETS", "active": false }), &c);
        assert!(sacred.unwrap_err().contains("protected (sacred) law")); // hard law refuses
    }
}
