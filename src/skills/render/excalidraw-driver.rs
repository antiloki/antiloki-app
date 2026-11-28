//! excalidraw-driver.rs — twin of skills/render/excalidraw-driver.mjs (db READ lane, single
//! default export `excalidrawDriver`). Same class as excalidraw-board.rs / excalidraw-emit.rs's
//! non-`buildExcalidraw` exports: this function's ENTIRE output is built by calling
//! `render_excalidraw_emit::text`/`rect`/`diamond`/`ellipse`/`arrow`/`narrativeBlock`, each of
//! which unconditionally calls the module-private OS-CSPRNG `id()` on every element on every
//! success path. A real id can never be byte-identical to Node's `crypto.randomBytes` across the
//! two lanes, so this unit is BESPOKE NATIVE: real, correct code, proven only by `#[cfg(test)]`
//! (checking shape/counts, not exact ids), no conform-db fixture / frontier entry / dispatch arm
//! (see excalidraw-board.rs's identical precedent + the crossing manifest for this module).

use rusqlite::Connection;
use serde_json::{json, Value};

fn extend_els(out: &mut Vec<Value>, v: Value) {
    if let Value::Array(els) = v { out.extend(els); }
}

/// `function safeCount(db, sql) { try { return db.prepare(sql).get().n; } catch { return 0; } }`
fn safe_count(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0)
}

fn box_(x: i64, y: i64, w: i64, h: i64, label: &str, fill: &str) -> Value {
    crate::render_excalidraw_emit::rect(&json!({
        "x": x, "y": y, "width": w, "height": h, "label": label, "fill": fill, "fontSize": 12
    }))
}

fn diamond_box(x: i64, y: i64, w: i64, h: i64, label: &str, fill: &str) -> Value {
    crate::render_excalidraw_emit::diamond(&json!({
        "x": x, "y": y, "width": w, "height": h, "label": label, "fill": fill, "fontSize": 12
    }))
}

fn ledger(x: i64, y: i64, w: i64, label: &str) -> Value {
    crate::render_excalidraw_emit::ellipse(&json!({
        "x": x, "y": y, "width": w, "height": 60, "label": label, "fill": "#f5f5f5", "fontSize": 11
    }))
}

fn arrow(x1: i64, y1: i64, x2: i64, y2: i64, label: Option<&str>, stroke: Option<&str>) -> Value {
    let mut opts = json!({ "x1": x1, "y1": y1, "x2": x2, "y2": y2 });
    if let Some(l) = label { opts["label"] = json!(l); }
    if let Some(s) = stroke { opts["stroke"] = json!(s); }
    crate::render_excalidraw_emit::arrow(&opts)
}

/// `export default function excalidrawDriver(input, context)`
pub fn excalidrawDriver(_input: &Value, db: &Connection) -> Result<Value, String> {
    // `const { db } = context ?? {}; if (!db) throw err.contextMissing(...)` — guard-throw, not
    // exercised per playbook gotcha #9 (fixture only covers the success path); Rust's db param is
    // already non-optional so the guard is unreachable here in the twin.

    let tick_count = safe_count(db, "SELECT COUNT(*) AS n FROM brain.driver_ticks");
    let fire_count = safe_count(db, "SELECT COUNT(*) AS n FROM brain.brain_fires");
    let decision_count = safe_count(db, "SELECT COUNT(*) AS n FROM brain.brain_decisions");
    let law_count = safe_count(db, "SELECT COUNT(*) AS n FROM laws.laws");
    let block_count = safe_count(db, "SELECT COUNT(*) AS n FROM laws.law_violations WHERE outcome='blocked'");

    let mut elements: Vec<Value> = Vec::new();

    extend_els(&mut elements, crate::render_excalidraw_emit::text(&json!({
        "x": 30, "y": 20, "text": "antiloki · driver loop architecture", "fontSize": 22, "color": "#1565c0"
    })));
    extend_els(&mut elements, crate::render_excalidraw_emit::text(&json!({
        "x": 30, "y": 55,
        "text": format!("{tick_count} ticks · {fire_count} fires · {decision_count} decisions · {law_count} active laws · {block_count} blocked"),
        "fontSize": 12, "color": "#6e7681",
    })));

    extend_els(&mut elements, crate::render_excalidraw_emit::narrativeBlock(&json!({
        "x": 30, "y": 90, "width": 1230,
        "text":
            "WHAT THIS DIAGRAM SHOWS — the heartbeat of antiloki.\n\
             \n\
             Most work in the substrate is autonomous: the driver ticks on a cadence (cron-paced), evaluates which actions are ready to fire (priority, locks, dependencies), sends each candidate through the law-check gate, dispatches the allowed ones, records the outcome, and loops. When a user types into chat, that is the same loop with one extra tick triggered by the message. Everything that ever runs in antiloki passes through this — there is no off-ramp. The diamond is the gate: in block mode, anything not from source=driver and not on the bootstrap allowlist gets refused and recorded in law_violations.\n\
             \n\
             HOW INFO TRANSFORMS toward the user goal: pending intent → driver wakes → candidates ranked → gate decides yes/no → action fires → outcome logged → next tick re-evaluates with the new state. The user's goal moves forward when the driver picks an action that advances it; the substrate is the scheduler.",
    })));

    const TOP: i64 = 340;
    const ROW1: i64 = TOP;
    const ROW2: i64 = TOP + 160;

    let tick = box_(80, ROW1, 180, 80, "TICK\n(cron + debounce)", "#e3f2fd");
    let eval_ = box_(310, ROW1, 200, 80, "EVALUATE\ncandidates (action-find\n+ priority + locks)", "#e1f5fe");
    let law_gate = diamond_box(550, ROW1, 200, 80, "LAW-CHECK\nDRIVER gate hook", "#fff9c4");
    let fire = box_(790, ROW1, 180, 80, "FIRE\naction-run dispatch", "#e8f5e9");
    let outcome = box_(1010, ROW1, 200, 80, "OUTCOME\nsuccess|error|blocked", "#fce4ec");

    let tick_ledger = ledger(100, ROW2, 160, "driver_ticks");
    let eval_ledger = ledger(330, ROW2, 160, "runtime_events\n(in_flight)");
    let law_ledger = ledger(560, ROW2, 180, "law_violations\n(blocked / warned)");
    let fire_ledger = ledger(810, ROW2, 160, "brain_fires");
    let outcome_ledger = ledger(1030, ROW2, 180, "sacred_timeline\n(if audited)");

    extend_els(&mut elements, tick);
    extend_els(&mut elements, eval_);
    extend_els(&mut elements, law_gate);
    extend_els(&mut elements, fire);
    extend_els(&mut elements, outcome);
    extend_els(&mut elements, tick_ledger);
    extend_els(&mut elements, eval_ledger);
    extend_els(&mut elements, law_ledger);
    extend_els(&mut elements, fire_ledger);
    extend_els(&mut elements, outcome_ledger);

    // Horizontal arrows across row 1
    extend_els(&mut elements, arrow(260, ROW1 + 40, 310, ROW1 + 40, None, None));
    extend_els(&mut elements, arrow(510, ROW1 + 40, 550, ROW1 + 40, None, None));
    extend_els(&mut elements, arrow(750, ROW1 + 40, 790, ROW1 + 40, Some("allowed"), None));
    extend_els(&mut elements, arrow(970, ROW1 + 40, 1010, ROW1 + 40, None, None));

    // Vertical drop from law-check down to law_violations when blocked
    extend_els(&mut elements, arrow(650, ROW1 + 80, 650, ROW2, Some("block"), Some("#c62828")));

    // Vertical drops to each ledger
    for (x, y) in [(170, ROW1 + 80), (410, ROW1 + 80), (880, ROW1 + 80), (1110, ROW1 + 80)] {
        extend_els(&mut elements, arrow(x, y, x, ROW2, None, Some("#9e9e9e")));
    }

    // Loop-back from outcome to tick
    extend_els(&mut elements, arrow(1110, ROW1, 1110, ROW1 - 40, None, Some("#1565c0")));
    extend_els(&mut elements, arrow(1110, ROW1 - 40, 170, ROW1 - 40, None, Some("#1565c0")));
    extend_els(&mut elements, arrow(170, ROW1 - 40, 170, ROW1, Some("next tick"), Some("#1565c0")));

    Ok(crate::render_excalidraw_emit::buildExcalidraw(
        &json!(elements),
        &json!({ "name": "antiloki-driver-loop" }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(db: &Connection) {
        db.execute_batch(
            "ATTACH DATABASE ':memory:' AS brain;
             ATTACH DATABASE ':memory:' AS laws;
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY);
             CREATE TABLE brain.brain_fires (id INTEGER PRIMARY KEY);
             CREATE TABLE brain.brain_decisions (id INTEGER PRIMARY KEY);
             CREATE TABLE laws.laws (id INTEGER PRIMARY KEY);
             CREATE TABLE laws.law_violations (id INTEGER PRIMARY KEY, outcome TEXT);"
        ).unwrap();
    }

    #[test]
    fn empty_state_counts_are_zero_and_shape_is_complete() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        let out = excalidrawDriver(&json!({}), &db).unwrap();
        assert_eq!(out["appState"]["name"], json!("antiloki-driver-loop"));
        let els = out["elements"].as_array().unwrap();
        assert!(els.iter().any(|e| e.get("text").and_then(Value::as_str) == Some("0 ticks · 0 fires · 0 decisions · 0 active laws · 0 blocked")));
        assert!(els.iter().any(|e| e.get("text").and_then(Value::as_str).map_or(false, |t| t.starts_with("WHAT THIS DIAGRAM SHOWS"))));
        assert!(els.iter().any(|e| e["type"] == json!("diamond")));
    }

    #[test]
    fn counts_reflect_row_counts_across_attached_dbs() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        db.execute_batch(
            "INSERT INTO brain.driver_ticks VALUES (1),(2),(3);
             INSERT INTO brain.brain_fires VALUES (1);
             INSERT INTO laws.laws VALUES (1),(2);
             INSERT INTO laws.law_violations (outcome) VALUES ('blocked'),('warned'),('blocked');"
        ).unwrap();
        let out = excalidrawDriver(&json!({}), &db).unwrap();
        let els = out["elements"].as_array().unwrap();
        assert!(els.iter().any(|e| e.get("text").and_then(Value::as_str) == Some("3 ticks · 1 fires · 0 decisions · 2 active laws · 2 blocked")));
    }

    #[test]
    fn missing_tables_are_treated_as_zero_count() {
        // no ATTACH at all — every qualified SELECT fails, safe_count must fall back to 0
        let db = Connection::open_in_memory().unwrap();
        let out = excalidrawDriver(&json!({}), &db).unwrap();
        let els = out["elements"].as_array().unwrap();
        assert!(els.iter().any(|e| e.get("text").and_then(Value::as_str) == Some("0 ticks · 0 fires · 0 decisions · 0 active laws · 0 blocked")));
    }

    #[test]
    fn arrows_and_ledgers_are_present() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        let out = excalidrawDriver(&json!({}), &db).unwrap();
        let els = out["elements"].as_array().unwrap();
        // 4 horizontal + 1 block-drop + 4 ledger-drops + 3 loop-back segments = 12
        let arrow_count = els.iter().filter(|e| e["type"] == json!("arrow")).count();
        assert_eq!(arrow_count, 12);
        let ellipse_count = els.iter().filter(|e| e["type"] == json!("ellipse")).count();
        assert_eq!(ellipse_count, 5);
    }
}
