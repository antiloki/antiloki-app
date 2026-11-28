//! excalidraw-pipeline.rs — twin of skills/render/excalidraw-pipeline.mjs (db READ lane, single
//! default export `excalidrawPipeline`). Same class as excalidraw-board.rs: this function's
//! ENTIRE output is built by calling `render_excalidraw_emit::rect`/`text`/`arrow`/
//! `narrativeBlock`, each of which unconditionally calls the module-private OS-CSPRNG `id()` on
//! every element on every success path. A real id can never be byte-identical to Node's
//! `crypto.randomBytes` across the two lanes, so this unit is BESPOKE NATIVE: real, correct code,
//! proven only by `#[cfg(test)]` (checking shape/lane grouping, not exact ids), no conform-db
//! fixture / frontier entry / dispatch arm (see excalidraw-board.rs's identical precedent + the
//! crossing manifest for this module).
//!
//! Throw fidelity (gotcha #9): the JS source `throw`s (context.db missing, card_slug missing,
//! card not found) — those guard paths are Err(String) here but are NOT fixture-exercised per
//! the playbook (fixture the success path only); this twin has no fixture at all regardless
//! (bespoke-native), so the Err(...) returns exist purely to mirror JS control flow faithfully.

use rusqlite::Connection;
use serde_json::{json, Value};

fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }

struct Phase { key: &'static str, label: &'static str, color: &'static str }

const PHASES: [Phase; 5] = [
    Phase { key: "pl_draft", label: "PL draft", color: "#fff8e1" },
    Phase { key: "tl_sign", label: "TL sign", color: "#fce4ec" },
    Phase { key: "dev_impl", label: "DEV impl", color: "#e3f2fd" },
    Phase { key: "qa_pass", label: "QA pass", color: "#e8f5e9" },
    Phase { key: "close", label: "CLOSE", color: "#f3e5f5" },
];

const BOX_W: f64 = 180.0;
const BOX_H: f64 = 90.0;
const GAP: f64 = 80.0;
const TOP: f64 = 360.0;
const LEFT: f64 = 60.0;

fn extend_els(out: &mut Vec<Value>, v: Value) {
    if let Value::Array(els) = v { out.extend(els); }
}

/// `export default function excalidrawPipeline(input, context)`
pub fn excalidrawPipeline(input: &Value, db: &Connection) -> Result<Value, String> {
    let card_slug = input.get("card_slug").and_then(Value::as_str).map(str::to_string);
    let card_slug = match card_slug {
        Some(s) if !s.is_empty() => s,
        _ => return Err("excalidraw-pipeline: card_slug is required".to_string()),
    };

    let cards = crate::db_json::query_rows(
        db,
        "SELECT slug, title, status FROM cards WHERE slug = ?",
        &[json!(card_slug)],
    ).map_err(|e| e.to_string())?;
    let card = cards.as_array().and_then(|a| a.first()).cloned();
    let card = match card {
        Some(c) => c,
        None => return Err(format!("card '{card_slug}' not found")),
    };
    let card_slug_out = str_qq(card.get("slug"));
    let card_status = str_qq(card.get("status"));

    let events = crate::db_json::query_rows(
        db,
        "SELECT phase, actor, ts FROM card_phase_events WHERE card_slug = ? ORDER BY id",
        &[json!(card_slug)],
    ).map_err(|e| e.to_string())?;
    let events: Vec<Value> = events.as_array().cloned().unwrap_or_default();
    let fired: std::collections::HashSet<String> = events.iter()
        .filter_map(|e| e.get("phase").and_then(Value::as_str).map(str::to_string))
        .collect();

    let mut elements: Vec<Value> = Vec::new();

    extend_els(&mut elements, crate::render_excalidraw_emit::text(&json!({
        "x": LEFT, "y": 30,
        "text": format!("pipeline · {card_slug_out}"),
        "fontSize": 22, "color": "#1565c0",
    })));
    extend_els(&mut elements, crate::render_excalidraw_emit::text(&json!({
        "x": LEFT, "y": 62,
        "text": format!("status={card_status} · {}/{} phases fired", events.len(), PHASES.len()),
        "fontSize": 13, "color": "#6e7681",
    })));

    extend_els(&mut elements, crate::render_excalidraw_emit::narrativeBlock(&json!({
        "x": LEFT, "y": 90, "width": 1380,
        "text": format!(
            "WHAT THIS DIAGRAM SHOWS — where the card \"{card_slug_out}\" is in its lifecycle, with timestamps.\n\
             \n\
             Each box is one of the five canonical phases that every card walks through. Colored + green-bordered boxes are phases that have actually fired (with the actor who fired them + the timestamp). Outlined-grey boxes are still pending. Arrows are colored green when the transition is complete, grey when not. The arrows between filled boxes are guarantees: the substrate physically refused to let the next phase fire until the previous one was recorded in card_phase_events.\n\
             \n\
             HOW INFO TRANSFORMS toward the user goal: read left-to-right to see how much of this card's work is real and audited. To understand WHO owns each phase and WHAT actions get invoked at each step in general, open lifecycle.excalidraw — the canonical abstract view."
        ),
    })));

    for i in 0..PHASES.len() {
        let p = &PHASES[i];
        let x = LEFT + (i as f64) * (BOX_W + GAP);
        let is_fired = fired.contains(p.key);
        let matching_event = events.iter().find(|e| e.get("phase").and_then(Value::as_str) == Some(p.key));
        let actor = matching_event.and_then(|e| e.get("actor")).map(|v| str_qq(Some(v))).unwrap_or_default();
        let stamp = matching_event.and_then(|e| e.get("ts")).map(|v| str_qq(Some(v))).unwrap_or_default();
        let subline = if is_fired { format!("{actor}\n{stamp}") } else { "(pending)".to_string() };

        extend_els(&mut elements, crate::render_excalidraw_emit::rect(&json!({
            "x": x, "y": TOP, "width": BOX_W, "height": BOX_H,
            "label": format!("{}\n{subline}", p.label),
            "fill": if is_fired { p.color } else { "#ffffff" },
            "stroke": if is_fired { "#2e7d32" } else { "#bdbdbd" },
            "fontSize": 12,
        })));

        if i < PHASES.len() - 1 {
            let next_fired = fired.contains(PHASES[i + 1].key);
            extend_els(&mut elements, crate::render_excalidraw_emit::arrow(&json!({
                "x1": x + BOX_W, "y1": TOP + BOX_H / 2.0,
                "x2": x + BOX_W + GAP, "y2": TOP + BOX_H / 2.0,
                "stroke": if is_fired && next_fired { "#2e7d32" } else { "#bdbdbd" },
            })));
        }
    }

    Ok(crate::render_excalidraw_emit::buildExcalidraw(
        &json!(elements),
        &json!({ "name": format!("antiloki-pipeline-{card_slug_out}") }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(db: &Connection) {
        db.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT, title TEXT, status TEXT);
             CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY, card_slug TEXT, phase TEXT, actor TEXT, ts TEXT)"
        ).unwrap();
    }

    #[test]
    fn missing_card_slug_errs() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        let err = excalidrawPipeline(&json!({}), &db).unwrap_err();
        assert!(err.contains("card_slug"));
    }

    #[test]
    fn card_not_found_errs() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        let err = excalidrawPipeline(&json!({ "card_slug": "NOPE" }), &db).unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn no_events_all_phases_pending() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        db.execute("INSERT INTO cards (slug, title, status) VALUES ('C-1', 'Title', 'backlog')", []).unwrap();
        let out = excalidrawPipeline(&json!({ "card_slug": "C-1" }), &db).unwrap();
        assert_eq!(out["appState"]["name"], json!("antiloki-pipeline-C-1"));
        let els = out["elements"].as_array().unwrap();
        assert!(els.iter().any(|e| e.get("text").and_then(Value::as_str) == Some("pipeline · C-1")));
        assert!(els.iter().any(|e| e.get("text").and_then(Value::as_str) == Some("status=backlog · 0/5 phases fired")));
        // 5 box rects, each with a "(pending)" label text bound
        let pending_count = els.iter().filter(|e| e.get("text").and_then(Value::as_str).map_or(false, |t| t.contains("(pending)"))).count();
        assert_eq!(pending_count, 5);
        // grey arrows between all 4 gaps
        let grey_arrows = els.iter().filter(|e| e["type"] == json!("arrow") && e["strokeColor"] == json!("#bdbdbd")).count();
        assert_eq!(grey_arrows, 4);
    }

    #[test]
    fn fired_phases_colored_and_arrows_green_between_consecutive_fired() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        db.execute("INSERT INTO cards (slug, title, status) VALUES ('C-2', 'Title', 'doing')", []).unwrap();
        db.execute("INSERT INTO card_phase_events (card_slug, phase, actor, ts) VALUES ('C-2','pl_draft','alice','2026-01-01T00:00:00.000Z')", []).unwrap();
        db.execute("INSERT INTO card_phase_events (card_slug, phase, actor, ts) VALUES ('C-2','tl_sign','bob','2026-01-02T00:00:00.000Z')", []).unwrap();
        let out = excalidrawPipeline(&json!({ "card_slug": "C-2" }), &db).unwrap();
        let els = out["elements"].as_array().unwrap();
        assert!(els.iter().any(|e| e.get("text").and_then(Value::as_str) == Some("status=doing · 2/5 phases fired")));
        assert!(els.iter().any(|e| e.get("text").and_then(Value::as_str) == Some("PL draft\nalice\n2026-01-01T00:00:00.000Z")));
        let green_arrows = els.iter().filter(|e| e["type"] == json!("arrow") && e["strokeColor"] == json!("#2e7d32")).count();
        assert_eq!(green_arrows, 1);
    }
}
