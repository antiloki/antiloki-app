//! excalidraw-meta.rs — twin of skills/render/excalidraw-meta.mjs (db READ lane, single default
//! export `excalidrawMeta`). Same class as excalidraw-board.rs / excalidraw-emit.rs: this
//! function's ENTIRE output is built by calling `render_excalidraw_emit::rect`/`text`/`arrow`/
//! `narrativeBlock`, each of which unconditionally calls the module-private OS-CSPRNG `id()` on
//! every element on every success path. A real id can never be byte-identical to Node's
//! `crypto.randomBytes` across the two lanes, so this unit is BESPOKE NATIVE: real, correct code,
//! proven only by `#[cfg(test)]` (checking shape/lane grouping + stat wiring, not exact ids), no
//! conform-db fixture / frontier entry / dispatch arm (see excalidraw-board.rs's identical
//! precedent + the crossing manifest for this module).
//!
//! `context.db` missing → `err.contextMissing('excalidraw-meta', 'db')` in JS ("excalidraw-meta:
//! context.db is required"); the Rust harness always supplies a live `Connection`, so — same as
//! excalidraw-board.rs — that guard-throw branch is not represented here (JS-only coverage).

use rusqlite::Connection;
use serde_json::{json, Value};

const TITLE_Y: i64 = 30;
const TOP: i64 = 340;
const ROW_H: i64 = 130;
const LANE_W: i64 = 980;
const LANE_X: i64 = 50;

struct Layer {
    label: &'static str,
    color: &'static str,
    detail: &'static str,
}

const LAYERS: [Layer; 5] = [
    Layer { label: "ENTRY POINTS", color: "#e3f2fd", detail: "MCP · CLI · BE chat · driver" },
    Layer { label: "GATE — action-run", color: "#fff9c4", detail: "law-check · driver-enforce · god-override" },
    Layer { label: "COMPOSE — skills", color: "#e8f5e9", detail: "atomic skills → actions (steps_json)" },
    Layer { label: "AUDIT", color: "#fce4ec", detail: "sacred_timeline (SHA-chain) · runtime_events · law_violations" },
    Layer { label: "LEARN — RAG memory", color: "#f3e5f5", detail: "memories · memory_links · embeddings · catalog" },
];

fn safe_count(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0)
}

fn extend_els(out: &mut Vec<Value>, v: Value) {
    if let Value::Array(els) = v { out.extend(els); }
}

/// `export default function excalidrawMeta(input, context)`
pub fn excalidrawMeta(_input: &Value, db: &Connection) -> Result<Value, String> {
    let actions = safe_count(db, "SELECT COUNT(*) AS n FROM actions");
    let skills_indexed = safe_count(db, "SELECT COUNT(*) AS n FROM memory.skills");
    let laws_active = safe_count(db, "SELECT COUNT(*) AS n FROM laws.laws");
    let blocked = safe_count(db, "SELECT COUNT(*) AS n FROM laws.law_violations WHERE outcome='blocked'");
    let cards = safe_count(db, "SELECT COUNT(*) AS n FROM cards");
    let phase_events = safe_count(db, "SELECT COUNT(*) AS n FROM card_phase_events");
    let sacred = safe_count(db, "SELECT COUNT(*) AS n FROM sacred_timeline");
    let runtime_events = safe_count(db, "SELECT COUNT(*) AS n FROM runtime_events");
    let memories = safe_count(db, "SELECT COUNT(*) AS n FROM memory.memories");
    let memory_links = safe_count(db, "SELECT COUNT(*) AS n FROM memory.memory_links");
    // stats.contracts is computed in JS but never referenced by the rendered output — transcribed
    // for parity with the JS `stats` object shape, silence the unused-field lint by discarding it.
    let _contracts = safe_count(db, "SELECT COUNT(*) AS n FROM contracts");

    let mut elements: Vec<Value> = Vec::new();

    extend_els(&mut elements, crate::render_excalidraw_emit::text(&json!({
        "x": LANE_X, "y": TITLE_Y,
        "text": "antiloki · system architecture (live)",
        "fontSize": 26, "color": "#1565c0",
    })));

    extend_els(&mut elements, crate::render_excalidraw_emit::text(&json!({
        "x": LANE_X, "y": TITLE_Y + 36,
        "text": format!(
            "{actions} actions · {skills_indexed} skills · {laws_active} laws ({blocked} blocked dispatches) · {cards} cards · {phase_events} phase events · {sacred} audit · {memories} memories"
        ),
        "fontSize": 13, "color": "#6e7681",
    })));

    extend_els(&mut elements, crate::render_excalidraw_emit::narrativeBlock(&json!({
        "x": LANE_X, "y": 80, "width": LANE_W,
        "text":
            "WHAT THIS DIAGRAM SHOWS — the substrate in one picture, top to bottom.\n\
             \n\
             antiloki is five layers stacked. ENTRY points (top) are where requests arrive: the FE chat, the MCP server (any Claude Code / Cursor / Cline client), the CLI, or the driver loop itself. Every request lands at the GATE, which is action-run + law-check — the only place where dispatch happens, the only place laws are consulted. Allowed requests flow into COMPOSE, where the action's steps_json wires together one or more atomic skills. As skills run, they leave receipts in AUDIT (sacred_timeline SHA chain, runtime_events log, phase events, law_violations). Outcomes feed LEARN, where corrections become new memories that the RAG arrow (right side) injects back into ENTRY on the next iteration.\n\
             \n\
             HOW INFO TRANSFORMS: external request → gated dispatch → composed skill chain → audited mutations → learned context → improved next request. The numbers in the header are live; regenerate this file and they update. This is the highest-level diagram in the substrate — when in doubt about the architecture, come back here.",
    })));

    for (i, l) in LAYERS.iter().enumerate() {
        let y: i64 = TOP + (i as i64) * ROW_H;
        extend_els(&mut elements, crate::render_excalidraw_emit::rect(&json!({
            "x": LANE_X, "y": y, "width": LANE_W, "height": ROW_H - 30,
            "label": format!("{}\n{}", l.label, l.detail),
            "fill": l.color, "fontSize": 14,
        })));

        if i < LAYERS.len() - 1 {
            extend_els(&mut elements, crate::render_excalidraw_emit::arrow(&json!({
                "x1": LANE_X + 100, "y1": y + ROW_H - 30,
                "x2": LANE_X + 100, "y2": y + ROW_H,
            })));
            extend_els(&mut elements, crate::render_excalidraw_emit::arrow(&json!({
                "x1": LANE_X + LANE_W - 100, "y1": y + ROW_H - 30,
                "x2": LANE_X + LANE_W - 100, "y2": y + ROW_H,
            })));
        }
    }

    let ann: [(usize, String); 5] = [
        (0, "source ∈ {mcp,cli,chat,driver}".to_string()),
        (1, format!("{blocked} blocked\n{laws_active} active laws")),
        (2, format!("{actions} actions\n{skills_indexed} skills")),
        (3, format!("{sacred} sacred\n{runtime_events} runtime\n{phase_events} phase")),
        (4, format!("{memories} memories\n{memory_links} links")),
    ];
    for (idx, text_val) in ann.iter() {
        let y: i64 = TOP + (*idx as i64) * ROW_H + 15;
        extend_els(&mut elements, crate::render_excalidraw_emit::text(&json!({
            "x": LANE_X + LANE_W + 20, "y": y, "text": text_val, "fontSize": 11, "color": "#6e7681",
        })));
    }

    let rag_y: i64 = TOP + 4 * ROW_H + (ROW_H - 30) / 2;
    let entry_y: i64 = TOP + (ROW_H - 30) / 2;
    extend_els(&mut elements, crate::render_excalidraw_emit::arrow(&json!({
        "x1": LANE_X + LANE_W + 200, "y1": rag_y,
        "x2": LANE_X + LANE_W + 200, "y2": entry_y,
        "label": "RAG inject", "stroke": "#9c27b0",
    })));
    extend_els(&mut elements, crate::render_excalidraw_emit::arrow(&json!({
        "x1": LANE_X + LANE_W + 200, "y1": entry_y,
        "x2": LANE_X + LANE_W, "y2": entry_y,
        "stroke": "#9c27b0",
    })));
    extend_els(&mut elements, crate::render_excalidraw_emit::arrow(&json!({
        "x1": LANE_X + LANE_W, "y1": rag_y,
        "x2": LANE_X + LANE_W + 200, "y2": rag_y,
        "stroke": "#9c27b0",
    })));

    Ok(crate::render_excalidraw_emit::buildExcalidraw(
        &json!(elements),
        &json!({ "name": "antiloki-meta-architecture" }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(db: &Connection) {
        db.execute_batch(
            "CREATE TABLE actions (id INTEGER PRIMARY KEY);
             CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT, title TEXT, status TEXT);
             CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY);
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY);
             CREATE TABLE runtime_events (id INTEGER PRIMARY KEY);
             CREATE TABLE contracts (id INTEGER PRIMARY KEY);"
        ).unwrap();
        db.execute_batch(
            "ATTACH DATABASE ':memory:' AS memory;
             ATTACH DATABASE ':memory:' AS laws;
             CREATE TABLE memory.skills (id INTEGER PRIMARY KEY);
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY);
             CREATE TABLE memory.memory_links (id INTEGER PRIMARY KEY);
             CREATE TABLE laws.laws (id INTEGER PRIMARY KEY);
             CREATE TABLE laws.law_violations (id INTEGER PRIMARY KEY, outcome TEXT);"
        ).unwrap();
    }

    fn texts(out: &Value) -> Vec<String> {
        out["elements"].as_array().unwrap().iter()
            .filter(|e| e["type"] == json!("text"))
            .map(|e| e["text"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn empty_substrate_renders_title_header_narrative_and_all_5_layers() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        let out = excalidrawMeta(&json!({}), &db).unwrap();
        assert_eq!(out["appState"]["name"], json!("antiloki-meta-architecture"));
        let t = texts(&out);
        assert!(t.contains(&"antiloki · system architecture (live)".to_string()));
        assert!(t.contains(&"0 actions · 0 skills · 0 laws (0 blocked dispatches) · 0 cards · 0 phase events · 0 audit · 0 memories".to_string()));
        assert!(t.iter().any(|s| s.starts_with("WHAT THIS DIAGRAM SHOWS")));
        for label in ["ENTRY POINTS", "GATE — action-run", "COMPOSE — skills", "AUDIT", "LEARN — RAG memory"] {
            assert!(t.iter().any(|s| s.starts_with(label)), "expected layer label starting with {label}");
        }
    }

    #[test]
    fn live_counts_feed_header_and_side_annotations() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        db.execute("INSERT INTO cards (slug, title, status) VALUES ('A-1','one','backlog'),('A-2','two','backlog')", []).unwrap();
        db.execute("INSERT INTO actions DEFAULT VALUES", []).unwrap();
        db.execute("INSERT INTO laws.laws DEFAULT VALUES", []).unwrap();
        db.execute("INSERT INTO laws.law_violations (outcome) VALUES ('blocked'), ('allowed')", []).unwrap();
        let out = excalidrawMeta(&json!({}), &db).unwrap();
        let t = texts(&out);
        assert!(t.contains(&"1 actions · 0 skills · 1 laws (1 blocked dispatches) · 2 cards · 0 phase events · 0 audit · 0 memories".to_string()));
        assert!(t.contains(&"1 blocked\n1 active laws".to_string()));
        assert!(t.contains(&"1 actions\n0 skills".to_string()));
    }

    #[test]
    fn side_annotation_zero_state_and_rag_arrow_present() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        let out = excalidrawMeta(&json!({}), &db).unwrap();
        let t = texts(&out);
        assert!(t.contains(&"source ∈ {mcp,cli,chat,driver}".to_string()));
        assert!(t.contains(&"0 sacred\n0 runtime\n0 phase".to_string()));
        assert!(t.contains(&"0 memories\n0 links".to_string()));
        assert!(t.contains(&"RAG inject".to_string()));
    }
}
