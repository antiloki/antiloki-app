//! pl-session-approve.rs — twin of skills/agents/pl/pl-session-approve.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `plSessionApprove` (default). Approval flow: with-audit(card-add) — INSERTs
//! the cards row + writes the sacred_timeline seal — then UPDATE pl_proposed_cards.status='approved',
//! UPDATE pl_sessions.status='done', then a best-effort card-phase-event-record(phase='pl_draft').
//! Two short-circuits: (1) bound-card-reuse — the session's approved_card_slug already equals the
//! target slug AND that card row already exists, so card-add is skipped entirely (avoids the UNIQUE
//! collision); (2) UNIQUE-constraint idempotent-recovery — when with-audit(card-add) fails with a
//! UNIQUE violation, a sibling proposed_card already 'approved' with the same slug means an earlier
//! attempt already created the card — supersede THIS proposed_card and close the session instead of
//! throwing. No sibling ⇒ rethrow as ALREADY_EXISTS.
//!
//! `approved_by` is a caller-suppliable actor (JS default `context?.actor ?? 'human:cli'`, but the
//! db lane never threads a context.actor, so the only reachable default is the literal 'human:cli'
//! unless input.approved_by is set) — threaded into `with-audit` as the actor via `withAuditAs`
//! (NOT plain `withAudit`, which hardcodes actor="unknown").
//!
//! JS's `err.contextMissing('pl-session-approve','db')` guard is unreachable here (db lane always
//! injects a Connection) — dropped per the with-audit/action-run precedent.
use rusqlite::Connection;
use serde_json::{Map, Value, json};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS `${v}` interpolation
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
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

fn frozen_iso() -> String { crate::clock::now_iso_nodb() }

pub fn plSessionApprove(input: &Value, db: &Connection) -> Result<Value, String> {
    let proposed_card_id = input.get("proposed_card_id").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&proposed_card_id)) {
        return Err("pl-session-approve: proposed_card_id is required".into());
    }

    // JS destructuring default: only an ABSENT key falls back — `approved_by = context?.actor ??
    // 'human:cli'`; the db lane never threads context.actor, so the reachable default is the literal.
    let approved_by_val = input.get("approved_by").cloned().unwrap_or(json!("human:cli"));
    let approved_by = approved_by_val.as_str().unwrap_or("human:cli").to_string();

    let slug_override = input.get("slug_override").cloned().unwrap_or(Value::Null);

    let proposed_rows = crate::db_json::query_rows(db, "SELECT * FROM pl_proposed_cards WHERE id = ?", &[proposed_card_id.clone()])?;
    let proposed = match proposed_rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("pl-session-approve: proposed card {} not found", tmpl(Some(&proposed_card_id)))),
    };
    let status = proposed.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status != "awaiting_approval" {
        return Err(format!("pl-session-approve: proposed card status='{status}', expected 'awaiting_approval'"));
    }

    // finalSlug = slug_override || proposed.slug_suggestion (JS `||` — falsy fallback, not just null).
    let final_slug: Value = if truthy(Some(&slug_override)) {
        slug_override
    } else {
        proposed.get("slug_suggestion").cloned().unwrap_or(Value::Null)
    };
    let final_slug_str = final_slug.as_str().unwrap_or("").to_string();

    let session_id = proposed.get("session_id").cloned().unwrap_or(Value::Null);
    let session_rows = crate::db_json::query_rows(db, "SELECT id, approved_card_slug FROM pl_sessions WHERE id = ?", &[session_id.clone()])?;
    let session = session_rows.as_array().and_then(|a| a.first()).cloned();

    let card_exists = {
        let rows = crate::db_json::query_rows(db, "SELECT 1 FROM cards WHERE slug = ?", &[final_slug.clone()])?;
        rows.as_array().map(|a| !a.is_empty()).unwrap_or(false)
    };
    // FIX-PL-FINALIZE-REUSES-BOUND-CARD-SLUG: when finalize reused an existing card's slug, skip
    // card-add to avoid the UNIQUE collision — just close out proposed_card + session.
    let is_bound_card_reuse = session
        .as_ref()
        .and_then(|s| s.get("approved_card_slug"))
        .map(|v| v == &final_slug)
        .unwrap_or(false)
        && card_exists;

    if is_bound_card_reuse {
        crate::db_json::run_write(
            db,
            &format!("UPDATE pl_proposed_cards SET status='approved', approved_by=?, approved_at={} WHERE id = ?", crate::clock::now_sql()),
            &[json!(approved_by), proposed_card_id.clone()],
        )?;
        crate::db_json::run_write(
            db,
            &format!("UPDATE pl_sessions SET status='done', approved_card_slug=?, updated_at={} WHERE id = ?", crate::clock::now_sql()),
            &[final_slug.clone(), session_id.clone()],
        )?;

        let phase_event_id = match crate::entities_card_phase_event_record::cardPhaseEventRecord(
            &json!({
                "card_slug": final_slug,
                "phase": "pl_draft",
                "actor": approved_by,
                "evidence": {
                    "source": "pl-session-approve", "session_id": session_id,
                    "proposed_card_id": proposed_card_id, "bound_card_reuse": true,
                },
            }),
            db,
        ) {
            Ok(ev) => nz(ev.get("id")).cloned().unwrap_or(Value::Null),
            Err(_) => Value::Null,
        };

        let mut out = Map::new();
        out.insert("approved_card_slug".into(), final_slug);
        out.insert("proposed_card_id".into(), proposed_card_id);
        out.insert("session_id".into(), session_id);
        out.insert("approved_by".into(), json!(approved_by));
        out.insert("approved_at".into(), json!(frozen_iso()));
        out.insert("phase_event_id".into(), phase_event_id);
        out.insert("bound_card_reuse".into(), json!(true));
        return Ok(Value::Object(out));
    }

    let audit_input = json!({
        "skill": "card-add",
        "wrappedInput": {
            "slug": final_slug,
            "title": proposed.get("title").cloned().unwrap_or(Value::Null),
            "status": "backlog",
            "body": proposed.get("body_md").cloned().unwrap_or(Value::Null),
        },
        "operation": "card.add",
        "entity_type": "card",
        "entity_id": final_slug,
    });

    if let Err(e) = crate::engine_with_audit::withAuditAs(&audit_input, db, &approved_by) {
        if e.contains("UNIQUE") {
            // Idempotency check: a sibling proposed_card with the same slug_suggestion already
            // 'approved' means the card was created by an earlier approval attempt. Supersede THIS
            // proposed_card and close the session pointing at the already-approved card.
            let sibling_rows = crate::db_json::query_rows(
                db,
                "SELECT id FROM pl_proposed_cards WHERE slug_suggestion = ? AND status = 'approved' AND id != ? ORDER BY id LIMIT 1",
                &[final_slug.clone(), proposed_card_id.clone()],
            )?;
            if let Some(sibling) = sibling_rows.as_array().and_then(|a| a.first()) {
                let sibling_id = sibling.get("id").cloned().unwrap_or(Value::Null);
                crate::db_json::run_write(db, "UPDATE pl_proposed_cards SET status='superseded' WHERE id = ?", &[proposed_card_id.clone()])?;
                crate::db_json::run_write(
                    db,
                    &format!("UPDATE pl_sessions SET status='done', approved_card_slug=?, updated_at={} WHERE id = ?", crate::clock::now_sql()),
                    &[final_slug.clone(), session_id.clone()],
                )?;

                let mut out = Map::new();
                out.insert("approved_card_slug".into(), final_slug);
                out.insert("proposed_card_id".into(), proposed_card_id);
                out.insert("session_id".into(), session_id);
                out.insert("approved_by".into(), json!(approved_by));
                out.insert("approved_at".into(), json!(frozen_iso()));
                out.insert("phase_event_id".into(), Value::Null);
                out.insert("idempotent_recovery".into(), json!(true));
                out.insert("superseded_by_proposed_card_id".into(), sibling_id);
                return Ok(Value::Object(out));
            }
            return Err(format!(
                "pl-session-approve: card slug '{final_slug_str}' already exists (and no sibling proposed_card claims it — pass slug_override)"
            ));
        }
        return Err(e);
    }

    crate::db_json::run_write(
        db,
        &format!("UPDATE pl_proposed_cards SET status='approved', approved_by=?, approved_at={} WHERE id = ?", crate::clock::now_sql()),
        &[json!(approved_by), proposed_card_id.clone()],
    )?;
    crate::db_json::run_write(
        db,
        &format!("UPDATE pl_sessions SET status='done', approved_card_slug=?, updated_at={} WHERE id = ?", crate::clock::now_sql()),
        &[final_slug.clone(), session_id.clone()],
    )?;

    // FEAT-AUTO-PHASE-EVENTS: best-effort — failures here must NOT roll back the approval.
    let phase_event_id = match crate::entities_card_phase_event_record::cardPhaseEventRecord(
        &json!({
            "card_slug": final_slug,
            "phase": "pl_draft",
            "actor": approved_by,
            "evidence": { "source": "pl-session-approve", "session_id": session_id, "proposed_card_id": proposed_card_id },
        }),
        db,
    ) {
        Ok(ev) => nz(ev.get("id")).cloned().unwrap_or(Value::Null),
        Err(_) => Value::Null,
    };

    let mut out = Map::new();
    out.insert("approved_card_slug".into(), final_slug);
    out.insert("proposed_card_id".into(), proposed_card_id);
    out.insert("session_id".into(), session_id);
    out.insert("approved_by".into(), json!(approved_by));
    out.insert("approved_at".into(), json!(frozen_iso()));
    out.insert("phase_event_id".into(), phase_event_id);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               slug TEXT UNIQUE, title TEXT, body_json TEXT, parent_slug TEXT,
               status TEXT, display_order REAL, created_at TEXT, updated_at TEXT
             );
             CREATE TABLE sacred_timeline (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT,
               before_json TEXT, after_json TEXT, payload TEXT,
               prev_print TEXT, digital_print TEXT,
               user_goal_id TEXT, exec_env TEXT, tick_id TEXT
             );
             CREATE TABLE pl_sessions (id INTEGER PRIMARY KEY, status TEXT, raw_intent TEXT, approved_card_slug TEXT, updated_at TEXT);
             CREATE TABLE pl_proposed_cards (id INTEGER PRIMARY KEY, session_id INTEGER, slug_suggestion TEXT, title TEXT, body_md TEXT, status TEXT, approved_by TEXT, approved_at TEXT);
             CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY, card_slug TEXT, phase TEXT, actor TEXT, evidence_json TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn normal_audited_insert_path() {
        let c = db0();
        c.execute_batch(
            "INSERT INTO pl_sessions (id, status, raw_intent, approved_card_slug, updated_at) VALUES (1, 'awaiting_approval', 'do the thing', NULL, '2025-12-01 00:00:00');
             INSERT INTO pl_proposed_cards (id, session_id, slug_suggestion, title, body_md, status) VALUES (1, 1, 'FEAT-NEW', 'New feature', 'body text', 'awaiting_approval');",
        )
        .unwrap();

        let r = plSessionApprove(&json!({ "proposed_card_id": 1 }), &c).unwrap();
        assert_eq!(r["approved_card_slug"], json!("FEAT-NEW"));
        assert_eq!(r["proposed_card_id"], json!(1));
        assert_eq!(r["session_id"], json!(1));
        assert_eq!(r["approved_by"], json!("human:cli"));
        assert_eq!(r["approved_at"], json!(frozen_iso()));
        assert!(r.get("bound_card_reuse").is_none());
        assert!(r["phase_event_id"].as_i64().is_some());

        let card = crate::db_json::query_rows(&c, "SELECT * FROM cards WHERE slug='FEAT-NEW'", &[]).unwrap();
        let card = card.as_array().unwrap().first().unwrap();
        assert_eq!(card["title"], json!("New feature"));
        assert_eq!(card["status"], json!("backlog"));

        let tl_count: i64 = c.query_row("SELECT COUNT(*) FROM sacred_timeline", [], |r| r.get(0)).unwrap();
        assert_eq!(tl_count, 1);

        let (pc_status, pc_by): (String, String) =
            c.query_row("SELECT status, approved_by FROM pl_proposed_cards WHERE id=1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(pc_status, "approved");
        assert_eq!(pc_by, "human:cli");

        let (s_status, s_slug): (String, String) =
            c.query_row("SELECT status, approved_card_slug FROM pl_sessions WHERE id=1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(s_status, "done");
        assert_eq!(s_slug, "FEAT-NEW");
    }

    #[test]
    fn bound_card_reuse_skips_card_add() {
        let c = db0();
        c.execute_batch(
            "INSERT INTO cards (slug, title, status) VALUES ('FEAT-BOUND', 'Bound card', 'backlog');
             INSERT INTO pl_sessions (id, status, raw_intent, approved_card_slug, updated_at) VALUES (2, 'awaiting_approval', 'x', 'FEAT-BOUND', '2025-12-01 00:00:00');
             INSERT INTO pl_proposed_cards (id, session_id, slug_suggestion, title, body_md, status) VALUES (2, 2, 'FEAT-BOUND', 'x', 'body', 'awaiting_approval');",
        )
        .unwrap();

        let r = plSessionApprove(&json!({ "proposed_card_id": 2 }), &c).unwrap();
        assert_eq!(r["bound_card_reuse"], json!(true));
        assert_eq!(r["approved_card_slug"], json!("FEAT-BOUND"));
        assert!(r["phase_event_id"].as_i64().is_some());

        let tl_count: i64 = c.query_row("SELECT COUNT(*) FROM sacred_timeline", [], |r| r.get(0)).unwrap();
        assert_eq!(tl_count, 0);
        let status: String = c.query_row("SELECT status FROM pl_proposed_cards WHERE id=2", [], |r| r.get(0)).unwrap();
        assert_eq!(status, "approved");
    }

    #[test]
    fn unique_recovery_supersedes_via_sibling() {
        let c = db0();
        c.execute_batch(
            "INSERT INTO cards (slug, title, status) VALUES ('FEAT-DUP', 'dup', 'backlog');
             INSERT INTO pl_sessions (id, status, raw_intent, approved_card_slug, updated_at) VALUES (3, 'awaiting_approval', 'x', NULL, '2025-12-01 00:00:00');
             INSERT INTO pl_proposed_cards (id, session_id, slug_suggestion, title, body_md, status) VALUES (10, 3, 'FEAT-DUP', 'sib', 'b', 'approved'), (11, 3, 'FEAT-DUP', 'this', 'b', 'awaiting_approval');",
        )
        .unwrap();

        let r = plSessionApprove(&json!({ "proposed_card_id": 11 }), &c).unwrap();
        assert_eq!(r["idempotent_recovery"], json!(true));
        assert_eq!(r["superseded_by_proposed_card_id"], json!(10));
        assert_eq!(r["approved_card_slug"], json!("FEAT-DUP"));
        assert_eq!(r["phase_event_id"], Value::Null);

        let status: String = c.query_row("SELECT status FROM pl_proposed_cards WHERE id=11", [], |r| r.get(0)).unwrap();
        assert_eq!(status, "superseded");
        let (s_status, s_slug): (String, String) =
            c.query_row("SELECT status, approved_card_slug FROM pl_sessions WHERE id=3", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(s_status, "done");
        assert_eq!(s_slug, "FEAT-DUP");
    }

    #[test]
    fn unique_with_no_sibling_throws_already_exists() {
        let c = db0();
        c.execute_batch(
            "INSERT INTO cards (slug, title, status) VALUES ('FEAT-TAKEN', 'taken', 'backlog');
             INSERT INTO pl_sessions (id, status, raw_intent, approved_card_slug, updated_at) VALUES (4, 'awaiting_approval', 'x', NULL, '2025-12-01 00:00:00');
             INSERT INTO pl_proposed_cards (id, session_id, slug_suggestion, title, body_md, status) VALUES (12, 4, 'FEAT-TAKEN', 't', 'b', 'awaiting_approval');",
        )
        .unwrap();

        let err = plSessionApprove(&json!({ "proposed_card_id": 12 }), &c).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn proposed_card_not_found() {
        let c = db0();
        let err = plSessionApprove(&json!({ "proposed_card_id": 999 }), &c).unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn wrong_status_guard() {
        let c = db0();
        c.execute_batch(
            "INSERT INTO pl_sessions (id, status, raw_intent, approved_card_slug, updated_at) VALUES (5, 'clarifying', 'x', NULL, '2025-12-01 00:00:00');
             INSERT INTO pl_proposed_cards (id, session_id, slug_suggestion, title, body_md, status) VALUES (13, 5, 'FEAT-X', 't', 'b', 'rejected');",
        )
        .unwrap();

        let err = plSessionApprove(&json!({ "proposed_card_id": 13 }), &c).unwrap_err();
        assert!(err.contains("expected 'awaiting_approval'"), "{err}");
    }
}
