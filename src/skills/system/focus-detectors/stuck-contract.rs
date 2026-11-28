//! stuck-contract.rs — twin of skills/system/focus-detectors/stuck-contract.mjs
//! (db-last helper lane: detect(card, db)). NAMES ARE CONTRACT: the export is `detect` (the JS
//! named default). Fires when a live (non-superseded) contract sits in a non-terminal status
//! (proposed, tl_approved, impl) with updated_at older than 1 hour. FROZEN CLOCK: the JS
//! source says datetime('now', ?) — the conform JS lane pins 'now' to 2026-01-01 00:00:00,
//! so this twin writes that literal in place.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

const STALE_HOURS: i64 = 1;

fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`

/// JS: `detect(card, db)` — a prepare/get failure (e.g. missing contracts table) is swallowed
/// by the JS try/catch → null; the Rust twin mirrors that on query error. `card_slug`
/// key-presence in fix option C mirrors JS (JSON.stringify drops undefined).
pub fn detect(card: &Value, db: &Connection) -> Value {
    let slug = card.get("slug").cloned().unwrap_or(Value::Null);
    let rows = match crate::db_json::query_rows(
        db,
        &format!("SELECT slug, status, updated_at FROM contracts\n      WHERE card_slug = ? AND COALESCE(superseded_by, '') = ''\n        AND status IN ('proposed', 'tl_approved', 'impl')\n        AND updated_at < datetime({}, ?)\n      ORDER BY id DESC LIMIT 1", crate::clock::now_sql_inner()),
        &[slug, json!(format!("-{STALE_HOURS} hours"))],
    ) {
        Ok(r) => r,
        Err(_) => return Value::Null,
    };
    let contract = match rows.as_array().and_then(|a| a.first()).cloned() {
        Some(c) => c,
        None => return Value::Null,
    };

    let reason_md = format!(
        "Contract {} is stuck in status='{}' (last update {}). It has been idle for at least {}h, which usually means an agent failed silently or the driver isn't picking it up. Force a tick (Pipeline \u{2192} t) or restart from the contract via Card view recover.",
        tmpl(contract.get("slug")),
        tmpl(contract.get("status")),
        tmpl(contract.get("updated_at")),
        STALE_HOURS,
    );

    let mut opt_c = Map::new();
    opt_c.insert("id".into(), json!("C"));
    opt_c.insert("label".into(), json!("Pause the card"));
    opt_c.insert("action".into(), json!("set_intent"));
    if let Some(s) = card.get("slug") {
        opt_c.insert("card_slug".into(), s.clone());
    }
    opt_c.insert("intent".into(), json!("paused"));

    json!({
        "kind": "stuck_contract",
        "detail": {
            "contract_slug": contract.get("slug").cloned().unwrap_or(Value::Null),
            "contract_status": contract.get("status").cloned().unwrap_or(Value::Null),
            "contract_updated_at": contract.get("updated_at").cloned().unwrap_or(Value::Null),
            "stale_hours_threshold": STALE_HOURS,
        },
        "reason_md": reason_md,
        "surfaces_in": ["board:red-badge", "pipeline:blocked-pane", "fixit:finding"],
        "fix_options": [
            { "id": "A", "label": "Force a driver tick", "action": "tick_now" },
            { "id": "B", "label": format!("Restart contract {} from failure", tmpl(contract.get("slug"))),
              "action": "restart_contract", "contract_slug": contract.get("slug").cloned().unwrap_or(Value::Null) },
            Value::Object(opt_c),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STALE: &str = "2025-12-20 10:00:00"; // well before frozen-now minus 1h
    const FRESH: &str = "2025-12-31 23:30:00"; // inside the last hour before frozen-now

    fn db_with(rows: &[(&str, &str, &str, Option<&str>, Option<&str>)]) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE contracts (id INTEGER PRIMARY KEY, slug TEXT, status TEXT, card_slug TEXT, superseded_by TEXT, updated_at TEXT)").unwrap();
        for (slug, status, card_slug, superseded_by, updated_at) in rows {
            c.execute(
                "INSERT INTO contracts (slug, status, card_slug, superseded_by, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![slug, status, card_slug, superseded_by, updated_at],
            )
            .unwrap();
        }
        c
    }

    #[test]
    fn missing_table_returns_null_like_js_catch() {
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&json!({"slug":"C1"}), &c), Value::Null);
    }

    #[test]
    fn non_matching_contracts_return_null() {
        assert_eq!(detect(&json!({"slug":"C1"}), &db_with(&[])), Value::Null);
        // other card / terminal status / superseded / fresh / NULL updated_at
        assert_eq!(detect(&json!({"slug":"C1"}), &db_with(&[("CT-9", "proposed", "OTHER", None, Some(STALE))])), Value::Null);
        assert_eq!(detect(&json!({"slug":"C1"}), &db_with(&[("CT-1", "done", "C1", None, Some(STALE))])), Value::Null);
        assert_eq!(detect(&json!({"slug":"C1"}), &db_with(&[("CT-1", "proposed", "C1", Some("CT-2"), Some(STALE))])), Value::Null);
        assert_eq!(detect(&json!({"slug":"C1"}), &db_with(&[("CT-1", "proposed", "C1", None, Some(FRESH))])), Value::Null);
        assert_eq!(detect(&json!({"slug":"C1"}), &db_with(&[("CT-1", "proposed", "C1", None, None)])), Value::Null);
    }

    #[test]
    fn stale_live_contract_fires_full_shape() {
        let db = db_with(&[("CT-1", "proposed", "C1", Some(""), Some(STALE))]);
        let f = detect(&json!({"slug":"C1","status":"doing"}), &db);
        assert_eq!(f["kind"], json!("stuck_contract"));
        assert_eq!(
            f["detail"],
            json!({"contract_slug":"CT-1","contract_status":"proposed","contract_updated_at":STALE,"stale_hours_threshold":1})
        );
        assert_eq!(
            f["reason_md"],
            json!("Contract CT-1 is stuck in status='proposed' (last update 2025-12-20 10:00:00). It has been idle for at least 1h, which usually means an agent failed silently or the driver isn't picking it up. Force a tick (Pipeline \u{2192} t) or restart from the contract via Card view recover.")
        );
        assert_eq!(f["surfaces_in"], json!(["board:red-badge", "pipeline:blocked-pane", "fixit:finding"]));
        assert_eq!(f["fix_options"][0], json!({"id":"A","label":"Force a driver tick","action":"tick_now"}));
        assert_eq!(f["fix_options"][1], json!({"id":"B","label":"Restart contract CT-1 from failure","action":"restart_contract","contract_slug":"CT-1"}));
        assert_eq!(f["fix_options"][2], json!({"id":"C","label":"Pause the card","action":"set_intent","card_slug":"C1","intent":"paused"}));
    }

    #[test]
    fn tl_approved_and_impl_fire_and_newest_wins() {
        for status in ["tl_approved", "impl"] {
            let f = detect(&json!({"slug":"C1"}), &db_with(&[("CT-1", status, "C1", None, Some(STALE))]));
            assert_eq!(f["detail"]["contract_status"], json!(status));
        }
        let db = db_with(&[
            ("CT-OLD", "proposed", "C1", None, Some(STALE)),
            ("CT-NEW", "impl", "C1", None, Some(STALE)),
        ]);
        let f = detect(&json!({"slug":"C1"}), &db);
        assert_eq!(f["detail"]["contract_slug"], json!("CT-NEW"));
        assert_eq!(f["fix_options"][1]["contract_slug"], json!("CT-NEW"));
    }

    #[test]
    fn missing_card_slug_stays_missing_in_option_c() {
        // unreachable via the SQL path in practice (NULL never matches card_slug), but the
        // key-presence contract is what JSON.stringify would enforce — pin it structurally:
        // detect with a card whose slug column matched via an explicit NULL-free row.
        let db = db_with(&[("CT-1", "impl", "C1", None, Some(STALE))]);
        let f = detect(&json!({"slug":"C1"}), &db);
        assert!(f["fix_options"][2].get("card_slug").is_some());
        // and a slugless card simply finds nothing (bind NULL) → null, same as JS
        assert_eq!(detect(&json!({}), &db), Value::Null);
    }
}
