//! excalidraw-bundles.rs — twin of skills/render/excalidraw-bundles.mjs (db READ lane, standard).
//! NAMES ARE CONTRACT: `excalidrawBundles` (default). Renders Sprint -> cards -> contracts as an
//! Excalidraw diagram, composing the shared `render_excalidraw_emit` factories (`rect`/`text`/
//! `arrow`/`narrativeBlock`/`buildExcalidraw`).
//!
//! BESPOKE NATIVE — same convention as `render_excalidraw_emit`'s element factories (see that
//! module's doc header). EVERY success path of this fn calls into those factories, which each
//! unconditionally call the module-private `id()` (`randomBytes(10)` OS CSPRNG, no seed/injection
//! seam) to stamp a fresh element id. That makes the WHOLE serialized output of this skill
//! non-byte-identical across independent JS/Rust invocations (unlike `buildExcalidraw` alone,
//! which is pure and IS fixture-tested there) — there is no way to isolate a deterministic
//! sub-slice here since every emitted rect/text/arrow embeds a random id. So: real, correct code
//! (real db reads, real layout math, real string composition matching the JS source field-for-
//! field other than ids), proven only by `#[cfg(test)]` (checking element counts/labels/db joins,
//! never exact ids), no conform-db fixture / frontier entry / dispatch arm.
//!
//! Object key order (gotcha #1) is inherited from `render_excalidraw_emit`'s factories — this
//! module never builds a raw element map itself, only assembles the `elements` array + top-level
//! excalidraw doc via those factories.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::render_excalidraw_emit as emit;

const SPRINT_X: f64 = 60.0;
const SPRINT_W: f64 = 240.0;
const SPRINT_H: f64 = 100.0;

const CARD_X: f64 = 380.0;
const CARD_W: f64 = 220.0;
const CARD_H: f64 = 70.0;
const CARD_GAP: f64 = 18.0;

const CONTRACT_X: f64 = 680.0;
const CONTRACT_W: f64 = 240.0;
const CONTRACT_H: f64 = 70.0;

const BUNDLE_NARRATIVE: &str = "WHAT THIS DIAGRAM SHOWS — how work is grouped into capacity-bounded batches in antiloki.\n\nA sprint is a named batch with a capacity (in story points) and a goal. Cards are committed to the sprint via sprint_items (left → middle arrow), each carrying an estimate. Each card may have one (or more) contracts attached (middle → right arrow), and each contract may declare a microtask plan — the engineering work cheap-tier DEV executes mechanically. The number on each contract is the microtask count.\n\nHOW INFO TRANSFORMS toward the user goal: vague intent → sprint commitment → card → contract (signed by TL) → microtasks → patches → merged work. This view shows the structural layer; lifecycle.excalidraw shows the temporal one. Together they answer: what is committed for this sprint, and how is each piece progressing?";

fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }

fn push_all(elements: &mut Vec<Value>, v: Value) { if let Value::Array(a) = v { elements.extend(a); } }

/// `export default function excalidrawBundles(input, context)`
pub fn excalidrawBundles(input: &Value, db: &Connection) -> Result<Value, String> {
    let empty = json!({});
    let inp = if input.is_null() { &empty } else { input };
    let sprint_slug = inp.get("sprint_slug");

    let sprint: Option<Value> = if truthy(sprint_slug) {
        crate::db_json::query_rows(db, "SELECT slug, goal_md, capacity_pts FROM sprints WHERE slug = ?", &[json!(str_qq(sprint_slug))])?
            .as_array().and_then(|a| a.first()).cloned()
    } else {
        crate::db_json::query_rows(db, "SELECT slug, goal_md, capacity_pts FROM sprints WHERE ended_at IS NULL ORDER BY id DESC LIMIT 1", &[])?
            .as_array().and_then(|a| a.first()).cloned()
    };

    let Some(sprint) = sprint else {
        let mut elements: Vec<Value> = Vec::new();
        push_all(&mut elements, emit::text(&json!({ "x": 30, "y": 20, "text": "antiloki · bundles", "fontSize": 22, "color": "#1565c0" })));
        push_all(&mut elements, emit::narrativeBlock(&json!({ "x": 30, "y": 60, "width": 1180, "text": BUNDLE_NARRATIVE })));
        push_all(&mut elements, emit::text(&json!({ "x": 30, "y": 320, "text": "(no sprints yet — file a sprint to populate this view)", "fontSize": 14, "color": "#9e9e9e" })));
        return Ok(emit::buildExcalidraw(&json!(elements), &json!({ "name": "antiloki-bundles-empty" })));
    };

    let items = crate::db_json::query_rows(
        db,
        "SELECT si.card_slug, si.phase, si.est_pts, c.title, c.status FROM sprint_items si LEFT JOIN cards c ON c.slug = si.card_slug WHERE si.sprint_slug = ? ORDER BY si.id",
        &[sprint.get("slug").cloned().unwrap_or(Value::Null)],
    )?.as_array().cloned().unwrap_or_default();

    let placeholders = if items.is_empty() { "''".to_string() } else { items.iter().map(|_| "?").collect::<Vec<_>>().join(",") };
    let contracts_sql = format!(
        "SELECT slug, card_slug, status, size, microtasks_json FROM contracts WHERE card_slug IN ({}) AND COALESCE(superseded_by, '') = ''",
        placeholders
    );
    let contract_params: Vec<Value> = items.iter().map(|i| i.get("card_slug").cloned().unwrap_or(Value::Null)).collect();
    let contracts = crate::db_json::query_rows(db, &contracts_sql, &contract_params)?.as_array().cloned().unwrap_or_default();

    let mut elements: Vec<Value> = Vec::new();
    push_all(&mut elements, emit::text(&json!({
        "x": 30, "y": 20,
        "text": format!("antiloki · sprint bundle · {}", tmpl(sprint.get("slug"))),
        "fontSize": 22, "color": "#1565c0",
    })));

    let goal: String = nz(sprint.get("goal_md")).and_then(Value::as_str).unwrap_or("").chars().take(80).collect();
    push_all(&mut elements, emit::text(&json!({
        "x": 30, "y": 55,
        "text": format!("goal: {} · capacity {}pts · {} items · {} contracts", goal, tmpl(sprint.get("capacity_pts")), items.len(), contracts.len()),
        "fontSize": 12, "color": "#6e7681",
    })));

    push_all(&mut elements, emit::narrativeBlock(&json!({ "x": 30, "y": 90, "width": 1180, "text": BUNDLE_NARRATIVE })));

    let top: f64 = 340.0;

    push_all(&mut elements, emit::rect(&json!({
        "x": SPRINT_X, "y": top, "width": SPRINT_W, "height": SPRINT_H,
        "label": format!("SPRINT\n{}\n{}pts", tmpl(sprint.get("slug")), tmpl(sprint.get("capacity_pts"))),
        "fill": "#e1f5fe", "fontSize": 13,
    })));

    let mut card_y = top - (items.len() as f64 * (CARD_H + CARD_GAP)) / 2.0 + SPRINT_H / 2.0;
    let mut card_y_by_card: HashMap<String, f64> = HashMap::new();
    for it in &items {
        let status = nz(it.get("status")).map(|v| tmpl(Some(v))).unwrap_or_else(|| "?".to_string());
        let label = format!("{}\n[{}] · {}pts", tmpl(it.get("card_slug")), status, tmpl(it.get("est_pts")));
        push_all(&mut elements, emit::rect(&json!({ "x": CARD_X, "y": card_y, "width": CARD_W, "height": CARD_H, "label": label, "fill": "#fff8e1", "fontSize": 11 })));
        push_all(&mut elements, emit::arrow(&json!({ "x1": SPRINT_X + SPRINT_W, "y1": top + SPRINT_H / 2.0, "x2": CARD_X, "y2": card_y + CARD_H / 2.0 })));
        let key = it.get("card_slug").and_then(Value::as_str).unwrap_or("").to_string();
        card_y_by_card.insert(key, card_y + CARD_H / 2.0);
        card_y += CARD_H + CARD_GAP;
    }

    for co in &contracts {
        let card_slug = co.get("card_slug").and_then(Value::as_str).unwrap_or("").to_string();
        let Some(&from_y) = card_y_by_card.get(&card_slug) else { continue };
        // `JSON.parse(co.microtasks_json || '[]').length` — falsy source (null/empty) defaults to
        // '[]'; a parse error is swallowed by the JS `catch {}`, leaving microtasks at its initial 0.
        let raw = co.get("microtasks_json").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("[]");
        let microtasks = serde_json::from_str::<Value>(raw).ok().and_then(|v| v.as_array().map(|a| a.len())).unwrap_or(0);
        let size = nz(co.get("size")).map(|v| tmpl(Some(v))).unwrap_or_else(|| "?".to_string());
        let label = format!("{}\n[{}] size={}\n{} microtasks", tmpl(co.get("slug")), tmpl(co.get("status")), size, microtasks);
        push_all(&mut elements, emit::rect(&json!({
            "x": CONTRACT_X, "y": from_y - CONTRACT_H / 2.0, "width": CONTRACT_W, "height": CONTRACT_H,
            "label": label, "fill": "#f3e5f5", "fontSize": 11,
        })));
        push_all(&mut elements, emit::arrow(&json!({ "x1": CARD_X + CARD_W, "y1": from_y, "x2": CONTRACT_X, "y2": from_y })));
    }

    Ok(emit::buildExcalidraw(&json!(elements), &json!({ "name": format!("antiloki-bundle-{}", tmpl(sprint.get("slug"))) })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(db: &Connection) {
        db.execute_batch(
            "CREATE TABLE sprints (id INTEGER PRIMARY KEY, slug TEXT, goal_md TEXT, capacity_pts INTEGER, ended_at TEXT); \
             CREATE TABLE sprint_items (id INTEGER PRIMARY KEY, sprint_slug TEXT, card_slug TEXT, phase TEXT, est_pts INTEGER); \
             CREATE TABLE cards (slug TEXT, title TEXT, status TEXT); \
             CREATE TABLE contracts (slug TEXT, card_slug TEXT, status TEXT, size TEXT, microtasks_json TEXT, superseded_by TEXT);",
        ).unwrap();
    }

    fn texts(out: &Value) -> Vec<String> {
        out["elements"].as_array().unwrap().iter()
            .filter(|e| e["type"] == json!("text"))
            .map(|e| e["originalText"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn no_sprints_yet_renders_empty_state() {
        let c = Connection::open_in_memory().unwrap();
        schema(&c);
        let out = excalidrawBundles(&json!({}), &c).unwrap();
        assert_eq!(out["type"], json!("excalidraw"));
        assert_eq!(out["appState"]["name"], json!("antiloki-bundles-empty"));
        let t = texts(&out);
        assert!(t.iter().any(|s| s == "antiloki · bundles"));
        assert!(t.iter().any(|s| s.contains("no sprints yet")));
    }

    #[test]
    fn picks_most_recent_open_sprint_when_no_sprint_slug_given() {
        let c = Connection::open_in_memory().unwrap();
        schema(&c);
        c.execute("INSERT INTO sprints (id, slug, goal_md, capacity_pts, ended_at) VALUES (1,'SP-OLD','old goal',10,'2026-01-01')", []).unwrap();
        c.execute("INSERT INTO sprints (id, slug, goal_md, capacity_pts, ended_at) VALUES (2,'SP-NEW','new goal',20,NULL)", []).unwrap();
        let out = excalidrawBundles(&json!({}), &c).unwrap();
        assert_eq!(out["appState"]["name"], json!("antiloki-bundle-SP-NEW"));
    }

    #[test]
    fn renders_sprint_with_cards_and_contracts_joins_and_skips_orphans_and_superseded() {
        let c = Connection::open_in_memory().unwrap();
        schema(&c);
        c.execute("INSERT INTO sprints (id, slug, goal_md, capacity_pts, ended_at) VALUES (1,'SP-1','ship the thing',30,NULL)", []).unwrap();
        c.execute("INSERT INTO cards (slug, title, status) VALUES ('CARD-A','A title','in-progress')", []).unwrap();
        c.execute("INSERT INTO sprint_items (id, sprint_slug, card_slug, phase, est_pts) VALUES (1,'SP-1','CARD-A','build',5)", []).unwrap();
        c.execute("INSERT INTO contracts (slug, card_slug, status, size, microtasks_json, superseded_by) VALUES ('CTR-A','CARD-A','signed','m','[{\"a\":1},{\"a\":2}]','')", []).unwrap();
        c.execute("INSERT INTO contracts (slug, card_slug, status, size, microtasks_json, superseded_by) VALUES ('CTR-ORPHAN','CARD-ORPHAN','signed','m','[]','')", []).unwrap();
        c.execute("INSERT INTO contracts (slug, card_slug, status, size, microtasks_json, superseded_by) VALUES ('CTR-SUPERSEDED','CARD-A','signed','m','[]','CTR-NEWER')", []).unwrap();

        let out = excalidrawBundles(&json!({ "sprint_slug": "SP-1" }), &c).unwrap();
        assert_eq!(out["appState"]["name"], json!("antiloki-bundle-SP-1"));
        let t = texts(&out);
        assert!(t.iter().any(|s| s == "antiloki · sprint bundle · SP-1"));
        assert!(t.iter().any(|s| s.contains("goal: ship the thing") && s.contains("capacity 30pts") && s.contains("1 items") && s.contains("1 contracts")));
        assert!(t.iter().any(|s| s.contains("SPRINT") && s.contains("SP-1") && s.contains("30pts")));
        assert!(t.iter().any(|s| s.contains("CARD-A") && s.contains("[in-progress]") && s.contains("5pts")));
        assert!(t.iter().any(|s| s.contains("CTR-A") && s.contains("[signed] size=m") && s.contains("2 microtasks")));
        assert!(!t.iter().any(|s| s.contains("CTR-ORPHAN")));
        assert!(!t.iter().any(|s| s.contains("CTR-SUPERSEDED")));
    }
}
