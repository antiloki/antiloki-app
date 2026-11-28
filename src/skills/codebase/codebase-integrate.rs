//! codebase-integrate.rs — twin of skills/codebase/codebase-integrate.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `codebaseIntegrate` (default). Close the atlas loop when a card FINISHES:
//! 1. RE-SYNC the atlas with the real built code via `codebaseRefresh({mode:'closure'})`
//!    (best-effort — a sync error degrades to `{ok:false,error}` and never blocks closure).
//! 2. FOLD the verified blueprint into the atlas — link each blueprint structural node to the
//!    atlas node it BECAME (same type+name+path) via `atlasFold`.
//! 3. Record the `atlas_integrated` phase event on the owning card (best-effort — missing
//!    contracts/card_phase_events tables, or no linked card, just skip the event).
//!
//! PARITY-DEFERRED: none — the only non-deterministic dependency (`ragSweep`'s embed call) lives
//! inside the already-twinned `codebase_codebase_refresh::codebaseRefresh`, called here exactly
//! as JS calls it. Everything in this file — the required-input guard, the best-effort sync
//! error-shaping, the blueprint→atlas node matching (max-version + STRUCTURAL filter), the fold
//! call, and the best-effort phase-event recording — is fully deterministic and ported faithfully.
use rusqlite::Connection;
use serde_json::{json, Value};

const STRUCTURAL: &[&str] = &["function", "class", "method", "component", "hook"];

fn posix(v: Option<&Value>) -> String {
    // `String(p ?? '').replace(/\\/g, '/')`
    let s = match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    };
    s.replace('\\', "/")
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) }
        }
        Some(_) => f64::NAN,
    }
}

// `Number(r.version) || 1` — NaN and 0 both collapse to 1.
fn version_or_1(v: Option<&Value>) -> f64 {
    let n = js_number(v);
    if n.is_nan() || n == 0.0 { 1.0 } else { n }
}

#[allow(non_snake_case)]
pub fn codebaseIntegrate(input: &Value, context: &Value, db: &Connection) -> Result<Value, String> {
    let slug_v = input.get("contract_slug").filter(|v| truthy(Some(v)));
    let Some(slug) = slug_v.cloned() else {
        return Err("codebase-integrate: contract_slug is required".into());
    };

    // 1. REFRESH — best-effort: a sync error degrades to `{ok:false, error}`, never blocks closure.
    let mut synced: Value = Value::Null;
    if !matches!(input.get("sync"), Some(Value::Bool(false))) {
        synced = match crate::codebase_codebase_refresh::codebaseRefresh(&json!({ "mode": "closure" }), context, db) {
            Ok(v) => v,
            Err(e) => {
                let msg: String = e.chars().take(200).collect();
                json!({ "ok": false, "error": msg })
            }
        };
    }

    // 2. FOLD — link each blueprint structural node to the atlas node it became (same name+path).
    let bp_rows = crate::db_json::query_rows(
        db,
        "SELECT id, origin, path, type, name, version FROM code_nodes WHERE origin LIKE 'blueprint:' || ? || '@%'",
        &[slug.clone()],
    )?;
    let bp_rows: Vec<Value> = bp_rows.as_array().cloned().unwrap_or_default();
    let max_v = bp_rows.iter().fold(1.0f64, |mx, r| mx.max(version_or_1(r.get("version"))));
    let projected: Vec<&Value> = bp_rows
        .iter()
        .filter(|r| version_or_1(r.get("version")) == max_v)
        .filter(|r| r.get("type").and_then(Value::as_str).map(|t| STRUCTURAL.contains(&t)).unwrap_or(false))
        .collect();

    let mut links: Vec<Value> = Vec::new();
    for p in &projected {
        let ty = p.get("type").cloned().unwrap_or(Value::Null);
        let name = p.get("name").cloned().unwrap_or(Value::Null);
        let path = json!(posix(p.get("path")));
        let at = crate::db_json::query_rows(
            db,
            "SELECT id FROM code_nodes WHERE origin = 'atlas' AND type = ? AND name = ? AND path = ? LIMIT 1",
            &[ty, name, path],
        )?;
        if let Some(row) = at.as_array().and_then(|a| a.first()) {
            links.push(json!({
                "blueprint_node": p.get("id").cloned().unwrap_or(Value::Null),
                "atlas_node": row.get("id").cloned().unwrap_or(Value::Null),
            }));
        }
    }

    let mut folded: i64 = 0;
    if !links.is_empty() {
        if let Ok(r) = crate::codebase_atlas_fold::atlasFold(&json!({ "links": links }), db) {
            folded = r.get("folded").and_then(Value::as_i64).unwrap_or(0);
        }
    }

    // Record the phase event so the driver's post-QA CLOSE chain sees this step is done.
    // Best-effort: contracts/card_phase_events absent (manual/test context) → skip the event.
    if let Ok(card_rows) = crate::db_json::query_rows(db, "SELECT card_slug FROM contracts WHERE slug = ?", &[slug.clone()]) {
        if let Some(card_slug) = card_rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("card_slug")).filter(|v| truthy(Some(v))) {
            let actor = context.get("actor").filter(|v| truthy(Some(v))).cloned().unwrap_or(json!("system:codebase-integrate"));
            let synced_ok = !matches!(synced.get("ok"), Some(Value::Bool(false)));
            let _ = crate::entities_card_phase_event_record::cardPhaseEventRecord(
                &json!({
                    "card_slug": card_slug,
                    "phase": "atlas_integrated",
                    "actor": actor,
                    "evidence": { "folded": folded, "links": links.len() as i64, "synced_ok": synced_ok },
                }),
                db,
            );
        }
    }

    Ok(json!({
        "contract_slug": slug,
        "synced": synced,
        "folded": folded,
        "links": links.len() as i64,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_schema() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, version INTEGER, became TEXT, from_blueprint TEXT);
             CREATE TABLE contracts (slug TEXT, card_slug TEXT);
             CREATE TABLE cards (slug TEXT);
             CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY, card_slug TEXT, phase TEXT, actor TEXT, evidence_json TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn requires_contract_slug() {
        let c = db_with_schema();
        let err = codebaseIntegrate(&json!({}), &json!({}), &c).unwrap_err();
        assert!(err.contains("contract_slug is required"));
    }

    #[test]
    fn folds_matching_blueprint_nodes_and_records_phase_event_when_card_linked() {
        let c = db_with_schema();
        c.execute_batch(
            "INSERT INTO code_nodes (id, origin, path, type, name, version) VALUES
                ('bp1', 'blueprint:C-1@1', 'src/a.mjs', 'function', 'foo', 1),
                ('bp2', 'blueprint:C-1@1', 'src/b.mjs', 'variable', 'bar', 1);
             INSERT INTO code_nodes (id, origin, path, type, name) VALUES
                ('at1', 'atlas', 'src/a.mjs', 'function', 'foo');
             INSERT INTO contracts (slug, card_slug) VALUES ('C-1', 'CARD-1');
             INSERT INTO cards (slug) VALUES ('CARD-1');",
        )
        .unwrap();

        let r = codebaseIntegrate(&json!({ "contract_slug": "C-1", "sync": false }), &json!({}), &c).unwrap();
        assert_eq!(r["contract_slug"], json!("C-1"));
        assert_eq!(r["synced"], Value::Null);
        assert_eq!(r["folded"], json!(1)); // only the 'function' type matches STRUCTURAL; 'variable' does not
        assert_eq!(r["links"], json!(1));

        let became: String = c.query_row("SELECT became FROM code_nodes WHERE id='bp1'", [], |row| row.get(0)).unwrap();
        assert_eq!(became, "at1");

        let phase: String = c.query_row("SELECT phase FROM card_phase_events WHERE card_slug='CARD-1'", [], |row| row.get(0)).unwrap();
        assert_eq!(phase, "atlas_integrated");
        let evidence: String = c.query_row("SELECT evidence_json FROM card_phase_events WHERE card_slug='CARD-1'", [], |row| row.get(0)).unwrap();
        assert_eq!(evidence, r#"{"folded":1,"links":1,"synced_ok":true}"#);
    }

    #[test]
    fn no_matching_atlas_node_yields_zero_links_and_no_phase_event_without_contract_row() {
        let c = db_with_schema();
        c.execute_batch(
            "INSERT INTO code_nodes (id, origin, path, type, name, version) VALUES
                ('bp1', 'blueprint:C-2@1', 'src/a.mjs', 'function', 'foo', 1);",
        )
        .unwrap();
        // no contracts row for C-2 → phase event skipped, no error
        let r = codebaseIntegrate(&json!({ "contract_slug": "C-2", "sync": false }), &json!({}), &c).unwrap();
        assert_eq!(r["folded"], json!(0));
        assert_eq!(r["links"], json!(0));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM card_phase_events", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn sync_true_default_surfaces_refresh_error_as_ok_false_object() {
        // No workspace-root / code_scope tables present → codebaseRefresh errors; best-effort
        // shaping must degrade to {ok:false, error} rather than propagating the error.
        let c = db_with_schema();
        let r = codebaseIntegrate(&json!({ "contract_slug": "C-3" }), &json!({}), &c).unwrap();
        assert_eq!(r["synced"]["ok"], json!(false));
        assert!(r["synced"]["error"].as_str().is_some());
    }
}
