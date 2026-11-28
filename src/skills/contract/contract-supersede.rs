//! contract-supersede.rs — twin of skills/contract/contract-supersede.mjs (db WRITE lane). NAMES ARE
//! CONTRACT: `contractSupersede` (default). Revise flow: a new draft (v2) lands via contract-add with
//! current_attempt+1, then the old row is marked superseded_by → the new slug. Idempotent re-dispatch
//! with the SAME new slug is a no-op success (benign concurrent-redraft race).
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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

pub fn contractSupersede(input: &Value, db: &Connection) -> Result<Value, String> {
    let empty = json!({});
    let input = if input.is_object() { input } else { &empty };

    let old_slug = input.get("old_slug").cloned().unwrap_or(Value::Null);
    let new_contract = input.get("new_contract").cloned().unwrap_or(Value::Null);

    if !truthy(Some(&old_slug)) { return Err("contract-supersede: old_slug is required".into()); }
    if !new_contract.is_object() { return Err("contract-supersede: new_contract is required".into()); }
    let new_slug = new_contract.get("slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&new_slug)) { return Err("contract-supersede: new_contract.slug missing".into()); }
    if new_slug == old_slug {
        return Err("contract-supersede: new_contract.slug must differ from old_slug (suggestion: append -2/-3)".into());
    }

    let old_rows = crate::db_json::query_rows(
        db,
        "SELECT slug, current_attempt, superseded_by, card_slug FROM contracts WHERE slug = ?",
        &[old_slug.clone()],
    )?;
    let old = match old_rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("contract-supersede: old contract '{}' not found", old_slug.as_str().unwrap_or(""))),
    };

    let old_superseded_by = old.get("superseded_by").cloned().unwrap_or(Value::Null);
    if truthy(Some(&old_superseded_by)) {
        // Idempotent re-dispatch: same computed next slug → benign no-op success.
        if old_superseded_by == new_slug {
            let existing_rows = crate::db_json::query_rows(
                db,
                "SELECT id, current_attempt FROM contracts WHERE slug = ?",
                &[new_slug.clone()],
            )?;
            let existing = existing_rows.as_array().and_then(|a| a.first());
            let old_ca = nz(old.get("current_attempt")).and_then(|v| v.as_i64()).unwrap_or(1);
            let fallback_ca = json!(old_ca + 1);
            let ca = existing
                .and_then(|r| r.get("current_attempt"))
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or(fallback_ca);
            let irid = existing
                .and_then(|r| r.get("id"))
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or(Value::Null);

            let mut out = Map::new();
            out.insert("old_slug".into(), old_slug.clone());
            out.insert("new_slug".into(), old_superseded_by.clone());
            out.insert("current_attempt".into(), ca);
            out.insert("first_pass_flag_now".into(), json!(0));
            out.insert("inserted_row_id".into(), irid);
            out.insert("already_superseded".into(), json!(true));
            return Ok(Value::Object(out));
        }
        return Err(format!(
            "contract-supersede: old contract '{}' is already superseded by '{}'",
            old_slug.as_str().unwrap_or(""),
            old_superseded_by.as_str().unwrap_or("")
        ));
    }

    // Persist the new contract row (HEAD of the chain) with current_attempt+1.
    let old_ca = nz(old.get("current_attempt")).and_then(|v| v.as_i64()).unwrap_or(1);
    let next_ca = old_ca + 1;
    let mut persist = new_contract.as_object().cloned().unwrap_or_default();
    persist.insert("slug".into(), new_slug.clone());
    persist.insert("current_attempt".into(), json!(next_ca));
    persist.insert("superseded_by".into(), Value::Null);
    persist.insert("status".into(), json!("proposed"));
    let inserted = crate::contract_contract_add::contractAdd(&Value::Object(persist), db)?;

    // Mark the old contract superseded. datetime('now') pinned to the frozen clock.
    crate::db_json::run_write(
        db,
        &format!("UPDATE contracts SET superseded_by = ?, status = 'superseded', updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
        &[new_slug.clone(), old_slug.clone()],
    )?;

    let irid = nz(inserted.get("id")).cloned().unwrap_or(Value::Null);
    let mut out = Map::new();
    out.insert("old_slug".into(), old_slug.clone());
    out.insert("new_slug".into(), new_slug.clone());
    out.insert("current_attempt".into(), json!(next_ca));
    out.insert("first_pass_flag_now".into(), json!(0));
    out.insert("inserted_row_id".into(), irid);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (\
               id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, title TEXT NOT NULL, \
               status TEXT NOT NULL DEFAULT 'proposed', template_kind TEXT, size TEXT, complexity INTEGER, \
               risk INTEGER, tier TEXT, card_slug TEXT, files_touched_json TEXT NOT NULL DEFAULT '[]', \
               acceptance_json TEXT NOT NULL DEFAULT '[]', naming_json TEXT NOT NULL DEFAULT '{}', \
               edge_cases_json TEXT NOT NULL DEFAULT '[]', must_obey_json TEXT NOT NULL DEFAULT '[]', \
               out_of_scope_json TEXT NOT NULL DEFAULT '[]', reference_artifacts_json TEXT NOT NULL DEFAULT '[]', \
               briefing_md TEXT, base_sha TEXT, current_attempt INTEGER NOT NULL DEFAULT 1, superseded_by TEXT, \
               signed_by TEXT, signed_at TEXT, tl_briefing_md TEXT, tl_evaluation_md TEXT, \
               microtasks_json TEXT NOT NULL DEFAULT '[]', goal_md TEXT, created_at TEXT, updated_at TEXT); \
             CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT, updated_at TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn fresh_supersede_inserts_and_marks_old() {
        let c = db();
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('C-1','backlog','2025-12-01 00:00:00')", []).unwrap();
        c.execute("INSERT INTO contracts (slug, title, status, card_slug, current_attempt) VALUES ('C-1-CONTRACT-1','t','proposed','C-1',1)", []).unwrap();

        let r = contractSupersede(
            &json!({ "old_slug": "C-1-CONTRACT-1",
                     "new_contract": { "slug": "C-1-CONTRACT-2", "title": "t v2", "card_slug": "C-1",
                                       "template_kind": "feature-add", "size": "S", "files_touched": ["a.mjs", "b.mjs"] } }),
            &c,
        )
        .unwrap();
        assert_eq!(r["old_slug"], json!("C-1-CONTRACT-1"));
        assert_eq!(r["new_slug"], json!("C-1-CONTRACT-2"));
        assert_eq!(r["current_attempt"], json!(2));
        assert_eq!(r["first_pass_flag_now"], json!(0));
        assert_eq!(r["inserted_row_id"], json!(2));
        assert!(r.as_object().unwrap().get("already_superseded").is_none());

        let (st, sb): (String, String) = c
            .query_row("SELECT status, superseded_by FROM contracts WHERE slug='C-1-CONTRACT-1'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(st, "superseded");
        assert_eq!(sb, "C-1-CONTRACT-2");
        let nca: i64 = c.query_row("SELECT current_attempt FROM contracts WHERE slug='C-1-CONTRACT-2'", [], |r| r.get(0)).unwrap();
        assert_eq!(nca, 2);
        let card_st: String = c.query_row("SELECT status FROM cards WHERE slug='C-1'", [], |r| r.get(0)).unwrap();
        assert_eq!(card_st, "doing");
    }

    #[test]
    fn redispatch_same_slug_is_noop() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, title, status, current_attempt, superseded_by) VALUES ('C-1-CONTRACT-1','t','superseded',1,'C-1-CONTRACT-2')", []).unwrap();
        c.execute("INSERT INTO contracts (slug, title, status, current_attempt) VALUES ('C-1-CONTRACT-2','t v2','proposed',2)", []).unwrap();

        let r = contractSupersede(
            &json!({ "old_slug": "C-1-CONTRACT-1",
                     "new_contract": { "slug": "C-1-CONTRACT-2", "title": "t v2" } }),
            &c,
        )
        .unwrap();
        assert_eq!(r["already_superseded"], json!(true));
        assert_eq!(r["new_slug"], json!("C-1-CONTRACT-2"));
        assert_eq!(r["current_attempt"], json!(2));
        assert_eq!(r["inserted_row_id"], json!(2));
        // no duplicate row created
        let cnt: i64 = c.query_row("SELECT COUNT(*) FROM contracts WHERE slug='C-1-CONTRACT-2'", [], |r| r.get(0)).unwrap();
        assert_eq!(cnt, 1);
    }

    #[test]
    fn redispatch_ghost_supersede_falls_back() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, title, status, current_attempt, superseded_by) VALUES ('C-1-CONTRACT-1','t','superseded',1,'C-1-CONTRACT-GHOST')", []).unwrap();

        let r = contractSupersede(
            &json!({ "old_slug": "C-1-CONTRACT-1",
                     "new_contract": { "slug": "C-1-CONTRACT-GHOST", "title": "x" } }),
            &c,
        )
        .unwrap();
        assert_eq!(r["already_superseded"], json!(true));
        assert_eq!(r["new_slug"], json!("C-1-CONTRACT-GHOST"));
        assert_eq!(r["current_attempt"], json!(2)); // (1 ?? 1) + 1
        assert!(r["inserted_row_id"].is_null());
    }
}
