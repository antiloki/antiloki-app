//! god-card-driver-intent-set.rs — twin of skills/agents/god/god-card-driver-intent-set.mjs (db
//! WRITE lane). NAMES ARE CONTRACT: `godCardDriverIntentSet` (default). Sets cards.driver_intent;
//! on intent='play' demotes other play-intent cards to 'paused' (FEAT-DOING-IS-A-QUEUE) and clears
//! the (injectable) halt-file if present (FEAT-PLAY-CLEARS-HALT); on intent='stopped' surfaces the
//! slug of any open contract. Guard throws (missing db/slug/intent, bad intent enum, card not
//! found, play on a terminal card) → Err, unfixtured (fixture ONLY the success path).
//!
//! FEAT-PLAY-CLEARS-HALT / VET: halt_path is injectable via input.halt_path. Fixtures pass a path
//! under a directory that provably does not exist on disk, so existsSync/Path::exists() is
//! deterministically false in BOTH lanes and cleared_halt stays false. The cleared_halt=true branch
//! needs a REAL pre-existing file the db-lane harness cannot create (no filesystem seeding lane) —
//! it is an honestly-uncovered branch, same class as a guard-throw: existsSync/unlinkSync ARE
//! exercised (the false path), just never observed with a present file.
//!
//! datetime('now') → frozen-clock literal '2026-01-01 00:00:00'.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// = JS `!x` (falsy) — used for the `!slug` / `!intent` guards.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS `${v}` template-literal interpolation (feeds the guard/error messages only).
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

const VALID_INTENTS: [&str; 4] = ["auto", "play", "paused", "stopped"];
// JS default: HALT_FILE_PATH = join(homedir(), '.antiloki', 'run', 'driver-halt') (or
// $ANTILOKI_HOME override). Never exercised by conform fixtures (see module doc VET) — this
// literal exists only so the fallback branch has a concrete (if platform-approximate) value.
const DEFAULT_HALT_PATH: &str = ".antiloki/run/driver-halt";

pub fn godCardDriverIntentSet(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug_v = input.get("slug").cloned();
    let intent_v = input.get("intent").cloned();

    // JS: `if (!db) throw err.contextMissing('god-card-driver-intent-set', 'db')` — the db lane
    // always hands the twin a live connection, so this guard cannot fire.
    if !truthy(slug_v.as_ref()) {
        return Err("god-card-driver-intent-set: slug is required".into());
    }
    if !truthy(intent_v.as_ref()) {
        return Err("god-card-driver-intent-set: intent is required".into());
    }
    let slug = slug_v.unwrap_or(Value::Null);
    let intent_str = intent_v.as_ref().and_then(|v| v.as_str());
    let intent = match intent_str {
        Some(i) if VALID_INTENTS.contains(&i) => i,
        _ => {
            return Err(format!(
                "god-card-driver-intent-set: intent must be one of {}; got '{}'",
                VALID_INTENTS.join("|"),
                tmpl(intent_v.as_ref())
            ))
        }
    };

    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, status, COALESCE(driver_intent, 'auto') AS driver_intent FROM cards WHERE slug = ?",
        &[slug.clone()],
    )?;
    let card = match rows.as_array().and_then(|a| a.first()).cloned() {
        Some(c) => c,
        None => {
            return Err(format!(
                "god-card-driver-intent-set: card '{}' not found",
                tmpl(Some(&slug))
            ))
        }
    };
    let status = card.get("status").and_then(|v| v.as_str()).unwrap_or("");

    // FEAT-CARD-INTENT-INVARIANTS: archived/done cards can't be "playing".
    if (status == "done" || status == "archived") && intent == "play" {
        return Err(format!(
            "god-card-driver-intent-set: card '{}' status='{}' — cannot set intent='play' on a terminal card. If you want to do more work, file a new card.",
            tmpl(Some(&slug)),
            tmpl(Some(&Value::String(status.to_string())))
        ));
    }

    crate::db_json::run_write(
        db,
        &format!("UPDATE cards SET driver_intent = ?, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
        &[json!(intent), slug.clone()],
    )?;

    // FEAT-DOING-IS-A-QUEUE: pressing "play" demotes every OTHER play-intent card to 'paused'.
    let mut demoted_count: i64 = 0;
    let mut cleared_halt = false;
    if intent == "play" {
        let (changes, _) = crate::db_json::run_write(
            db,
            &format!("UPDATE cards SET driver_intent = 'paused', updated_at = {} WHERE slug != ? AND COALESCE(driver_intent, 'auto') = 'play' AND status NOT IN ('done', 'archived')", crate::clock::now_sql()),
            &[slug.clone()],
        )?;
        demoted_count = changes as i64;

        // FEAT-PLAY-CLEARS-HALT: an explicit "play this card" overrides a global pause.
        let halt_path = input
            .get("halt_path")
            .filter(|v| !v.is_null())
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| DEFAULT_HALT_PATH.to_string());
        if std::path::Path::new(&halt_path).exists() {
            // JS: try { unlinkSync(haltPath); cleared_halt = true; } catch { /* race, goal met */ }
            if std::fs::remove_file(&halt_path).is_ok() {
                cleared_halt = true;
            }
        }
    }

    // If stopping, surface the open contract (if any) for the calling workflow to reject.
    let mut open_contract_slug = Value::Null;
    if intent == "stopped" {
        let ct = crate::db_json::query_rows(
            db,
            "SELECT slug FROM contracts WHERE card_slug = ? AND status IN ('proposed', 'impl') AND COALESCE(superseded_by, '') = '' ORDER BY id DESC LIMIT 1",
            &[slug.clone()],
        )?;
        open_contract_slug = ct
            .as_array()
            .and_then(|a| a.first())
            .and_then(|r| r.get("slug"))
            .cloned()
            .unwrap_or(Value::Null);
    }

    let previous = card.get("driver_intent").cloned().unwrap_or(Value::Null);
    let changed = previous.as_str() != Some(intent);

    let mut out = Map::new();
    out.insert("slug".into(), slug);
    out.insert("intent".into(), json!(intent));
    out.insert("previous".into(), previous);
    out.insert("changed".into(), json!(changed));
    out.insert("open_contract_slug".into(), open_contract_slug);
    out.insert("demoted_count".into(), json!(demoted_count));
    out.insert("cleared_halt".into(), json!(cleared_halt));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT, driver_intent TEXT, updated_at TEXT);
             CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, card_slug TEXT, status TEXT, superseded_by TEXT);",
        )
        .unwrap();
        c
    }
    fn seed_card(c: &Connection, slug: &str, status: &str, driver_intent: Option<&str>) {
        c.execute(
            "INSERT INTO cards (slug, status, driver_intent, updated_at) VALUES (?, ?, ?, '2025-12-01 00:00:00')",
            rusqlite::params![slug, status, driver_intent],
        )
        .unwrap();
    }
    fn seed_contract(c: &Connection, id: i64, slug: &str, card_slug: &str, status: &str, superseded_by: &str) {
        c.execute(
            "INSERT INTO contracts (id, slug, card_slug, status, superseded_by) VALUES (?, ?, ?, ?, ?)",
            rusqlite::params![id, slug, card_slug, status, superseded_by],
        )
        .unwrap();
    }

    #[test]
    fn idempotent_auto_to_auto_unchanged() {
        let c = db0();
        seed_card(&c, "C1", "backlog", Some("auto"));
        let r = godCardDriverIntentSet(&json!({ "slug": "C1", "intent": "auto" }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "slug": "C1", "intent": "auto", "previous": "auto", "changed": false, "open_contract_slug": null, "demoted_count": 0, "cleared_halt": false })
        );
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["slug", "intent", "previous", "changed", "open_contract_slug", "demoted_count", "cleared_halt"]);
    }

    #[test]
    fn null_driver_intent_coalesces_to_auto() {
        let c = db0();
        seed_card(&c, "C2", "backlog", None);
        let r = godCardDriverIntentSet(&json!({ "slug": "C2", "intent": "paused" }), &c).unwrap();
        assert_eq!(r["previous"], json!("auto"));
        assert_eq!(r["changed"], json!(true));
        let after: String = c.query_row("SELECT driver_intent FROM cards WHERE slug='C2'", [], |row| row.get(0)).unwrap();
        assert_eq!(after, "paused");
    }

    #[test]
    fn play_demotes_other_play_cards_excluding_terminal_statuses() {
        let c = db0();
        seed_card(&c, "TARGET", "backlog", Some("auto"));
        seed_card(&c, "OTHER-PLAY", "backlog", Some("play"));
        seed_card(&c, "OTHER-DONE-PLAY", "done", Some("play"));
        seed_card(&c, "OTHER-ARCHIVED-PLAY", "archived", Some("play"));
        seed_card(&c, "OTHER-AUTO", "backlog", Some("auto"));
        let halt_path = "C:/__antiloki_crossing_fixture_halt_never_exists__/driver-halt";
        let r = godCardDriverIntentSet(&json!({ "slug": "TARGET", "intent": "play", "halt_path": halt_path }), &c).unwrap();
        assert_eq!(r["demoted_count"], json!(1));
        assert_eq!(r["cleared_halt"], json!(false));
        assert_eq!(r["previous"], json!("auto"));
        assert_eq!(r["changed"], json!(true));
        let target: String = c.query_row("SELECT driver_intent FROM cards WHERE slug='TARGET'", [], |row| row.get(0)).unwrap();
        assert_eq!(target, "play");
        let other: String = c.query_row("SELECT driver_intent FROM cards WHERE slug='OTHER-PLAY'", [], |row| row.get(0)).unwrap();
        assert_eq!(other, "paused");
        let done: String = c.query_row("SELECT driver_intent FROM cards WHERE slug='OTHER-DONE-PLAY'", [], |row| row.get(0)).unwrap();
        assert_eq!(done, "play"); // terminal status excluded from demotion
        let archived: String = c.query_row("SELECT driver_intent FROM cards WHERE slug='OTHER-ARCHIVED-PLAY'", [], |row| row.get(0)).unwrap();
        assert_eq!(archived, "play"); // terminal status excluded from demotion
        let auto: String = c.query_row("SELECT driver_intent FROM cards WHERE slug='OTHER-AUTO'", [], |row| row.get(0)).unwrap();
        assert_eq!(auto, "auto"); // not driver_intent='play', never matched
    }

    #[test]
    fn play_with_no_other_play_cards_demotes_nothing() {
        let c = db0();
        seed_card(&c, "C7", "backlog", Some("auto"));
        let halt_path = "C:/__antiloki_crossing_fixture_halt_never_exists__/driver-halt";
        let r = godCardDriverIntentSet(&json!({ "slug": "C7", "intent": "play", "halt_path": halt_path }), &c).unwrap();
        assert_eq!(r["demoted_count"], json!(0));
        assert_eq!(r["cleared_halt"], json!(false));
    }

    #[test]
    fn stopped_surfaces_the_most_recent_open_contract() {
        let c = db0();
        seed_card(&c, "C4", "impl", Some("auto"));
        seed_contract(&c, 1, "C4-K1", "C4", "proposed", "");
        seed_contract(&c, 2, "C4-K2", "C4", "impl", "");
        let r = godCardDriverIntentSet(&json!({ "slug": "C4", "intent": "stopped" }), &c).unwrap();
        assert_eq!(r["open_contract_slug"], json!("C4-K2"));
        assert_eq!(r["demoted_count"], json!(0));
        assert_eq!(r["cleared_halt"], json!(false));
    }

    #[test]
    fn stopped_with_no_open_contract_reports_null() {
        let c = db0();
        seed_card(&c, "C5", "impl", Some("auto"));
        seed_contract(&c, 3, "C5-K1", "C5", "done", "");
        seed_contract(&c, 4, "C5-K2", "C5", "impl", "C5-K3");
        let r = godCardDriverIntentSet(&json!({ "slug": "C5", "intent": "stopped" }), &c).unwrap();
        assert_eq!(r["open_contract_slug"], Value::Null);
    }

    #[test]
    fn terminal_card_allows_non_play_intents() {
        let c = db0();
        seed_card(&c, "C6", "done", Some("play"));
        let r = godCardDriverIntentSet(&json!({ "slug": "C6", "intent": "paused" }), &c).unwrap();
        assert_eq!(r["previous"], json!("play"));
        assert_eq!(r["changed"], json!(true));
        assert_eq!(r["intent"], json!("paused"));
    }

    #[test]
    fn guards_reject_bad_input() {
        let c = db0();
        seed_card(&c, "X", "backlog", Some("auto"));
        assert!(godCardDriverIntentSet(&json!({ "intent": "play" }), &c).is_err()); // missing slug
        assert!(godCardDriverIntentSet(&json!({ "slug": "X" }), &c).is_err()); // missing intent
        assert!(godCardDriverIntentSet(&json!({ "slug": "X", "intent": "banana" }), &c).is_err()); // bad enum
        assert!(godCardDriverIntentSet(&json!({ "slug": "NOPE", "intent": "play" }), &c).is_err()); // not found
        seed_card(&c, "DONE-X", "done", Some("auto"));
        assert!(godCardDriverIntentSet(&json!({ "slug": "DONE-X", "intent": "play" }), &c).is_err()); // terminal + play
    }
}
