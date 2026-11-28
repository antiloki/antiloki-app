//! sandbox-phase-set.rs — twin of skills/entities/sandbox-phase-set.mjs (db WRITE lane, attached
//! `memory` db). NAMES ARE CONTRACT: `sandboxPhaseSet` (default) + `PHASES` (data).
//! Transitions a card through the sandbox lifecycle research → contract → impl → review → closed.
//! Refuses non-forward transitions unless force=true; a forced regression writes a best-effort
//! 'correction' memory. Feature card: FEAT-SANDBOX-LIFECYCLE.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

pub const PHASES: [&str; 5] = ["research", "contract", "impl", "review", "closed"];

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
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

pub fn sandboxPhaseSet(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('sandbox-phase-set','db') when no db — the db lane always injects one.
    let card_slug_v = input.get("card_slug");
    if !truthy(card_slug_v) {
        return Err("sandbox-phase-set: card_slug is required".into());
    }
    let card_slug = card_slug_v.expect("truthy implies present").clone();
    let phase_v = input.get("phase");
    // PHASES.includes(phase): only an exact member string passes; anything else throws enumVal
    let phase = phase_v.and_then(|v| v.as_str()).unwrap_or("");
    let Some(target_idx) = PHASES.iter().position(|p| *p == phase) else {
        return Err(format!(
            "sandbox-phase-set: phase must be one of: {} (got '{}')",
            PHASES.join(", "),
            tmpl(phase_v)
        ));
    };
    let target_idx = target_idx as i64;
    let force = truthy(input.get("force"));

    let card_rows = crate::db_json::query_rows(db, "SELECT slug FROM cards WHERE slug = ?", &[card_slug.clone()])?;
    if card_rows.as_array().map_or(true, |a| a.is_empty()) {
        return Err(format!("sandbox-phase-set: card '{}' not found", tmpl(Some(&card_slug))));
    }

    let existing = crate::db_json::query_rows(db, "SELECT current_phase FROM card_sandbox WHERE card_slug = ?", &[card_slug.clone()])?
        .as_array()
        .and_then(|a| a.first())
        .cloned();
    // Some(value) when a row exists (column value, possibly Null); None when no row
    let current_phase = existing.as_ref().and_then(|r| r.get("current_phase")).cloned();
    // JS: existing ? PHASES.indexOf(existing.current_phase) : -1 — a stored phase outside the enum is -1 too
    let current_idx: i64 = current_phase
        .as_ref()
        .and_then(|v| v.as_str())
        .and_then(|s| PHASES.iter().position(|p| *p == s))
        .map_or(-1, |i| i as i64);

    if existing.is_some() && target_idx < current_idx && !force {
        return Err(format!(
            "sandbox-phase-set: regressing from '{}' to '{}' requires force=true",
            tmpl(current_phase.as_ref()),
            phase
        ));
    }

    if existing.is_some() {
        // frozen-clock lane: JS datetime('now') runs through the harness proxy → same literal
        crate::db_json::run_write(
            db,
            &format!("UPDATE card_sandbox SET current_phase=?, updated_at={} WHERE card_slug=?", crate::clock::now_sql()),
            &[json!(phase), card_slug.clone()],
        )?;
    } else {
        crate::db_json::run_write(
            db,
            "INSERT INTO card_sandbox (card_slug, current_phase) VALUES (?, ?)",
            &[card_slug.clone(), json!(phase)],
        )?;
    }

    let is_regression = existing.is_some() && target_idx < current_idx;
    if is_regression && force {
        // JS wraps this in try/catch (memory write is best-effort) — swallow any failure
        let _ = crate::db_json::run_write(
            db,
            "INSERT INTO memory.memories (type, title, situation, guidance, naive_response, tags_json, source_card, created_by)
        VALUES ('correction', ?, ?, ?, ?, '[\"sandbox\"]', ?, 'sandbox-phase-set:auto')",
            &[
                json!(format!("Sandbox regression on {}", tmpl(Some(&card_slug)))),
                json!(format!("Sandbox phase regressed from '{}' back to '{}'", tmpl(current_phase.as_ref()), phase)),
                json!(format!("Investigate what went wrong at '{}' before re-progressing", tmpl(current_phase.as_ref()))),
                json!(format!("Tried to continue from '{}' without addressing the regression cause", tmpl(current_phase.as_ref()))),
                card_slug.clone(),
            ],
        );
    }

    let mut out = Map::new();
    out.insert("card_slug".into(), card_slug);
    out.insert("previous_phase".into(), current_phase.unwrap_or(Value::Null)); // existing?.current_phase ?? null
    out.insert("current_phase".into(), json!(phase));
    // JS: `existing && targetIdx < currentIdx` — no row leaves isRegression undefined → key OMITTED
    if existing.is_some() {
        out.insert("is_regression".into(), json!(is_regression));
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY);
            CREATE TABLE card_sandbox (card_slug TEXT PRIMARY KEY REFERENCES cards(slug), current_phase TEXT NOT NULL, started_at TEXT NOT NULL DEFAULT ('2026-01-01 00:00:00'), updated_at TEXT NOT NULL DEFAULT ('2026-01-01 00:00:00'));
            ATTACH ':memory:' AS memory;
            CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL, situation TEXT NOT NULL, guidance TEXT NOT NULL, naive_response TEXT, rationale TEXT, tags_json TEXT NOT NULL DEFAULT '[]', source_card TEXT, confidence REAL NOT NULL DEFAULT 0.5, validated INTEGER NOT NULL DEFAULT 0, created_by TEXT NOT NULL);
            INSERT INTO cards (slug) VALUES ('CARD-A');",
        )
        .unwrap();
        c
    }
    fn seed_phase(c: &Connection, phase: &str) {
        c.execute("INSERT INTO card_sandbox (card_slug, current_phase, started_at, updated_at) VALUES ('CARD-A', ?1, '2025-12-20 09:00:00', '2025-12-20 09:00:00')", [phase]).unwrap();
    }

    #[test]
    fn fresh_set_inserts_and_omits_is_regression() {
        let c = db();
        let r = sandboxPhaseSet(&json!({ "card_slug": "CARD-A", "phase": "research" }), &c).unwrap();
        // key order + is_regression omission are part of the contract
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"card_slug":"CARD-A","previous_phase":null,"current_phase":"research"}"#
        );
        let row = crate::db_json::query_rows(&c, "SELECT * FROM card_sandbox", &[]).unwrap()[0].clone();
        assert_eq!(row["current_phase"], json!("research"));
    }

    #[test]
    fn forward_and_same_phase_update_without_force() {
        let c = db();
        seed_phase(&c, "research");
        let r = sandboxPhaseSet(&json!({ "card_slug": "CARD-A", "phase": "impl" }), &c).unwrap();
        assert_eq!(r["previous_phase"], json!("research"));
        assert_eq!(r["is_regression"], json!(false));
        let row = crate::db_json::query_rows(&c, "SELECT * FROM card_sandbox", &[]).unwrap()[0].clone();
        assert_eq!(row["current_phase"], json!("impl"));
        assert_eq!(row["updated_at"], json!("2026-01-01 00:00:00")); // frozen literal
        // same phase is not a regression (targetIdx == currentIdx)
        let r2 = sandboxPhaseSet(&json!({ "card_slug": "CARD-A", "phase": "impl" }), &c).unwrap();
        assert_eq!(r2["is_regression"], json!(false));
    }

    #[test]
    fn regression_needs_force_and_writes_correction_memory() {
        let c = db();
        seed_phase(&c, "review");
        let e = sandboxPhaseSet(&json!({ "card_slug": "CARD-A", "phase": "contract" }), &c).unwrap_err();
        assert!(e.contains("regressing from 'review' to 'contract' requires force=true"), "{e}");
        let r = sandboxPhaseSet(&json!({ "card_slug": "CARD-A", "phase": "contract", "force": true }), &c).unwrap();
        assert_eq!(r["is_regression"], json!(true));
        let mem = crate::db_json::query_rows(&c, "SELECT * FROM memory.memories", &[]).unwrap()[0].clone();
        assert_eq!(mem["type"], json!("correction"));
        assert_eq!(mem["title"], json!("Sandbox regression on CARD-A"));
        assert_eq!(mem["situation"], json!("Sandbox phase regressed from 'review' back to 'contract'"));
        assert_eq!(mem["guidance"], json!("Investigate what went wrong at 'review' before re-progressing"));
        assert_eq!(mem["naive_response"], json!("Tried to continue from 'review' without addressing the regression cause"));
        assert_eq!(mem["tags_json"], json!("[\"sandbox\"]"));
        assert_eq!(mem["source_card"], json!("CARD-A"));
        assert_eq!(mem["created_by"], json!("sandbox-phase-set:auto"));
    }

    #[test]
    fn unknown_stored_phase_counts_as_minus_one_and_forward_force_writes_no_memory() {
        let c = db();
        seed_phase(&c, "weird");
        let r = sandboxPhaseSet(&json!({ "card_slug": "CARD-A", "phase": "research", "force": true }), &c).unwrap();
        assert_eq!(r["previous_phase"], json!("weird"));
        assert_eq!(r["is_regression"], json!(false)); // 0 < -1 is false — not a regression
        let mems = crate::db_json::query_rows(&c, "SELECT * FROM memory.memories", &[]).unwrap();
        assert_eq!(mems.as_array().unwrap().len(), 0);
    }

    #[test]
    fn memory_write_is_best_effort() {
        // no attached memory db at all: forced regression still succeeds (JS try/catch swallow)
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY);
            CREATE TABLE card_sandbox (card_slug TEXT PRIMARY KEY, current_phase TEXT NOT NULL, updated_at TEXT);
            INSERT INTO cards (slug) VALUES ('CARD-A');
            INSERT INTO card_sandbox (card_slug, current_phase) VALUES ('CARD-A', 'closed');",
        )
        .unwrap();
        let r = sandboxPhaseSet(&json!({ "card_slug": "CARD-A", "phase": "impl", "force": true }), &c).unwrap();
        assert_eq!(r["is_regression"], json!(true));
    }

    #[test]
    fn guards_err() {
        let c = db();
        assert!(sandboxPhaseSet(&json!({}), &c).unwrap_err().contains("card_slug is required"));
        let e = sandboxPhaseSet(&json!({ "card_slug": "CARD-A", "phase": "shipping" }), &c).unwrap_err();
        assert_eq!(e, "sandbox-phase-set: phase must be one of: research, contract, impl, review, closed (got 'shipping')");
        let e2 = sandboxPhaseSet(&json!({ "card_slug": "NOPE", "phase": "research" }), &c).unwrap_err();
        assert_eq!(e2, "sandbox-phase-set: card 'NOPE' not found");
    }
}
