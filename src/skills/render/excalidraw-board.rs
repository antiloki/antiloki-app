//! excalidraw-board.rs — twin of skills/render/excalidraw-board.mjs (db READ lane, single default
//! export `excalidrawBoard`). Same class as excalidraw-emit.rs's non-`buildExcalidraw` exports:
//! this function's ENTIRE output is built by calling `render_excalidraw_emit::rect`/`text`/
//! `narrativeBlock`, each of which unconditionally calls the module-private OS-CSPRNG `id()` on
//! every element on every success path. A real id can never be byte-identical to Node's
//! `crypto.randomBytes` across the two lanes, so this unit is BESPOKE NATIVE: real, correct code,
//! proven only by `#[cfg(test)]` (checking shape/lane grouping, not exact ids), no conform-db
//! fixture / frontier entry / dispatch arm (see excalidraw-emit.rs's identical precedent + the
//! crossing manifest for this module).
//!
//! Frozen clock (playbook): `new Date().toISOString().slice(0, 10)` → the literal `"2026-01-01"`.

use rusqlite::Connection;
use serde_json::{json, Value};

fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
// = JS Number(v ?? undefined) restricted to what `input.limit_per_lane` can hold, then the
// non-negative-integer coercion Array.prototype.slice(0, end) applies to its `end` argument.
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } }

const STATUS_ORDER: [&str; 4] = ["backlog", "doing", "done", "archived"];

fn status_color(status: &str) -> &'static str {
    match status {
        "backlog" => "#fff8e1",
        "doing" => "#e3f2fd",
        "done" => "#e8f5e9",
        "archived" => "#f5f5f5",
        _ => "#ffffff",
    }
}

fn extend_els(out: &mut Vec<Value>, v: Value) {
    if let Value::Array(els) = v { out.extend(els); }
}

/// `export default function excalidrawBoard(input, context)`
pub fn excalidrawBoard(input: &Value, db: &Connection) -> Result<Value, String> {
    // `const { limit_per_lane = 12 } = input ?? {}` — default fires only when the key is
    // genuinely absent (JS `undefined`), NOT when it is present-and-null.
    let limit_per_lane: i64 = match input.get("limit_per_lane") {
        None => 12,
        Some(v) => {
            let n = js_number(Some(v));
            if n.is_nan() { 0 } else { n.max(0.0) as i64 }
        }
    };

    let cards = crate::db_json::query_rows(db, "SELECT slug, title, status FROM cards ORDER BY id", &[])
        .map_err(|e| e.to_string())?;
    let cards: Vec<Value> = cards.as_array().cloned().unwrap_or_default();

    let mut elements: Vec<Value> = Vec::new();

    extend_els(&mut elements, crate::render_excalidraw_emit::text(&json!({
        "x": 30, "y": 20, "text": "antiloki · board (live)", "fontSize": 22, "color": "#1565c0"
    })));

    extend_els(&mut elements, crate::render_excalidraw_emit::narrativeBlock(&json!({
        "x": 30, "y": 60, "width": 1280,
        "text":
            "WHAT THIS DIAGRAM SHOWS — the live state of work in antiloki, right now.\n\
             \n\
             Each card is a unit of work. Lanes are status: backlog (filed, not started), doing (in flight), done (shipped), archived (closed for historical reasons). The position of a card across this board is what the substrate considers true at the moment of regeneration. To see WHY a card is where it is — who moved it, when, with what evidence — open lifecycle.excalidraw for the canonical phase flow or pipeline.excalidraw with a specific card_slug for its individual journey.\n\
             \n\
             HOW INFO TRANSFORMS toward the user goal: regenerate this file (one command) to see the current state of the team's commitments. The diagram is a derived view, not the source of truth — the cards table in state.sqlite is. If something here surprises you, query the table directly."
    })));

    for (li, status) in STATUS_ORDER.iter().enumerate() {
        let lane_x: i64 = 30 + (li as i64) * (280 + 30);
        let status_cards: Vec<&Value> = cards.iter()
            .filter(|c| c.get("status").and_then(Value::as_str) == Some(*status))
            .collect();
        let lane_cards: Vec<&Value> = status_cards.iter().take(limit_per_lane as usize).cloned().collect();
        let lane_height: i64 = 50 + (lane_cards.len() as i64) * (60 + 12) + 14;

        extend_els(&mut elements, crate::render_excalidraw_emit::rect(&json!({
            "x": lane_x, "y": 280,
            "width": 280, "height": lane_height,
            "label": format!("{} ({})", status, status_cards.len()),
            "fill": status_color(status),
            "fontSize": 16,
        })));

        let mut cy: i64 = 280 + 50;
        for card in &lane_cards {
            let slug = str_qq(card.get("slug"));
            let title: String = str_qq(card.get("title")).chars().take(32).collect();
            extend_els(&mut elements, crate::render_excalidraw_emit::rect(&json!({
                "x": lane_x + 14, "y": cy,
                "width": 280 - (14 * 2),
                "height": 60,
                "label": format!("{}\n{}", slug, title),
                "fill": "#ffffff", "fontSize": 11,
            })));
            cy += 60 + 12;
        }

        let truncated = status_cards.len() as i64 - lane_cards.len() as i64;
        if truncated > 0 {
            extend_els(&mut elements, crate::render_excalidraw_emit::text(&json!({
                "x": lane_x + 14, "y": cy,
                "text": format!("… +{truncated} more"),
                "fontSize": 11, "color": "#6e7681",
            })));
        }
    }

    Ok(crate::render_excalidraw_emit::buildExcalidraw(
        &json!(elements),
        &json!({ "name": "antiloki-board-2026-01-01" }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(db: &Connection) {
        db.execute_batch("CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT, title TEXT, status TEXT)").unwrap();
    }

    #[test]
    fn empty_board_has_title_narrative_and_all_four_empty_lanes() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        let out = excalidrawBoard(&json!({}), &db).unwrap();
        assert_eq!(out["appState"]["name"], json!("antiloki-board-2026-01-01"));
        let els = out["elements"].as_array().unwrap();
        // title text + narrative (rect+text) + 4 lane rects, no card rects, no truncation hints
        assert!(els.iter().any(|e| e["text"] == json!("antiloki · board (live)")));
        assert!(els.iter().any(|e| e["type"] == json!("text") && e.get("text").and_then(Value::as_str).map_or(false, |t| t.starts_with("WHAT THIS DIAGRAM SHOWS"))));
        for status in STATUS_ORDER {
            assert!(els.iter().any(|e| e["type"] == json!("text") && e["text"] == json!(format!("{status} (0)"))));
        }
    }

    #[test]
    fn groups_cards_by_status_lane() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        db.execute("INSERT INTO cards (slug, title, status) VALUES ('A-1','one','backlog'),('A-2','two','doing'),('A-3','three','doing')", []).unwrap();
        let out = excalidrawBoard(&json!({}), &db).unwrap();
        let els = out["elements"].as_array().unwrap();
        assert!(els.iter().any(|e| e["text"] == json!("backlog (1)")));
        assert!(els.iter().any(|e| e["text"] == json!("doing (2)")));
        assert!(els.iter().any(|e| e.get("text").and_then(Value::as_str) == Some("A-1\none")));
        assert!(els.iter().any(|e| e.get("text").and_then(Value::as_str) == Some("A-2\ntwo")));
    }

    #[test]
    fn title_truncated_to_32_chars_in_card_label() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        let long_title = "x".repeat(50);
        db.execute("INSERT INTO cards (slug, title, status) VALUES ('T-1', ?1, 'backlog')", [&long_title]).unwrap();
        let out = excalidrawBoard(&json!({}), &db).unwrap();
        let els = out["elements"].as_array().unwrap();
        let label = els.iter().find_map(|e| e.get("text").and_then(Value::as_str).filter(|t| t.starts_with("T-1\n"))).unwrap();
        let title_part = label.strip_prefix("T-1\n").unwrap();
        assert_eq!(title_part.chars().count(), 32);
    }

    #[test]
    fn limit_per_lane_truncates_and_emits_more_hint() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        for i in 0..5 {
            db.execute("INSERT INTO cards (slug, title, status) VALUES (?1, 'x', 'backlog')", [format!("C-{i}")]).unwrap();
        }
        let out = excalidrawBoard(&json!({ "limit_per_lane": 2 }), &db).unwrap();
        let els = out["elements"].as_array().unwrap();
        assert!(els.iter().any(|e| e["text"] == json!("backlog (5)")));
        assert!(els.iter().any(|e| e.get("text").and_then(Value::as_str) == Some("… +3 more")));
    }

    #[test]
    fn no_truncation_hint_when_lane_within_limit() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        db.execute("INSERT INTO cards (slug, title, status) VALUES ('S-1','x','backlog')", []).unwrap();
        let out = excalidrawBoard(&json!({}), &db).unwrap();
        let els = out["elements"].as_array().unwrap();
        assert!(!els.iter().any(|e| e.get("text").and_then(Value::as_str).map_or(false, |t| t.contains("more"))));
    }
}
