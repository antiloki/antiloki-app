//! excalidraw-rag.rs — twin of skills/render/excalidraw-rag.mjs (db READ lane, single default
//! export `excalidrawRag`). Same class as excalidraw-board.rs / excalidraw-driver.rs: this
//! function's ENTIRE output is built by calling `render_excalidraw_emit::rect`/`text`/`arrow`/
//! `narrativeBlock`/`buildExcalidraw`, each of which unconditionally calls the module-private
//! OS-CSPRNG `id()` on every element on every success path. A real id can never be byte-identical
//! to Node's `crypto.randomBytes` across the two lanes, so this unit is BESPOKE NATIVE: real,
//! correct code, proven only by `#[cfg(test)]` (checking shape/counts, not exact ids), no
//! conform-db fixture / frontier entry / dispatch arm (see excalidraw-driver.rs's identical
//! precedent + the crossing manifest for this module).
//!
//! Frozen clock (playbook): `new Date().toISOString().slice(0, 10)` → the literal `"2026-01-01"`.

use rusqlite::Connection;
use serde_json::{json, Value};

fn extend_els(out: &mut Vec<Value>, v: Value) {
    if let Value::Array(els) = v { out.extend(els); }
}

/// `function safeCount(db, sql) { try { return db.prepare(sql).get().n; } catch { return 0; } }`
fn safe_count(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0)
}

struct Step {
    label: &'static str,
    fill: &'static str,
    skill: &'static str,
    tables: &'static str,
}

const BOX_W: i64 = 200;
const BOX_H: i64 = 110;
const GAP: i64 = 50;
const LEFT: i64 = 40;

const C_TRIGGER: &str = "#bbdefb";
const C_EMBED: &str = "#e1bee7";
const C_SEARCH: &str = "#d1c4e9";
const C_FILTER: &str = "#c5cae9";
const C_FETCH: &str = "#bbdefb";
const C_FORMAT: &str = "#b2dfdb";
const C_AI: &str = "#fff9c4";
const C_LOG: &str = "#ffe0b2";
const C_OUTCOME: &str = "#ffccbc";
const C_LEARN: &str = "#c8e6c9";
const C_LOOP: &str = "#9c27b0";

/// `export default function excalidrawRag(input, context)`
pub fn excalidrawRag(_input: &Value, db: &Connection) -> Result<Value, String> {
    // `const { db } = context ?? {}; if (!db) throw err.contextMissing(...)` — guard-throw, not
    // exercised per playbook gotcha #9 (fixture only covers the success path); Rust's db param is
    // already non-optional so the guard is unreachable here in the twin.

    let memories = safe_count(db, "SELECT COUNT(*) AS n FROM memory.memories");
    let memory_links = safe_count(db, "SELECT COUNT(*) AS n FROM memory.memory_links");
    let embeddings = safe_count(db, "SELECT COUNT(*) AS n FROM memory.embeddings");
    let catalog_embed = safe_count(db, "SELECT COUNT(*) AS n FROM memory.catalog_embeddings");
    let memory_uses = safe_count(db, "SELECT COUNT(*) AS n FROM memory.memory_uses");
    let outcomes = safe_count(db, "SELECT COUNT(*) AS n FROM memory.memory_use_outcomes");
    let corrections = safe_count(db, "SELECT COUNT(*) AS n FROM memory.memories WHERE type='correction'");

    let mut e: Vec<Value> = Vec::new();

    extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
        "x": 40, "y": 30,
        "text": "antiloki · RAG + self-learning pipeline (linear flow)",
        "fontSize": 24, "color": "#7b1fa2",
    })));
    extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
        "x": 40, "y": 64,
        "text": format!(
            "{memories} memories ({corrections} corrections) · {memory_links} links · {embeddings}+{catalog_embed} vectors · {memory_uses} uses · {outcomes} outcomes"
        ),
        "fontSize": 12, "color": "#6e7681",
    })));
    extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
        "x": 40, "y": 84,
        "text": "every box is a real skill + the table(s) it touches. follow the arrows to walk the RAG loop.",
        "fontSize": 11, "color": "#9e9e9e",
    })));

    extend_els(&mut e, crate::render_excalidraw_emit::narrativeBlock(&json!({
        "x": 40, "y": 100, "width": 1880,
        "text":
            "WHAT THIS DIAGRAM SHOWS — how antiloki learns from every interaction and gets quietly smarter.\n\
             \n\
             When a skill needs context to answer well, it does NOT just dump everything it knows into the LLM. It searches its accumulated memory the same way a human consults experience. The user's message is encoded into a vector (top row, left side). The substrate finds the closest matches in catalog_embeddings via cosine similarity, filters by type and validation, and walks the memory_links graph to pull related context. That bundle is formatted into a prompt section and injected into the LLM call. Then — and this is the part most \"RAG demos\" skip — the substrate watches what happens. memory_uses records WHICH memories were consulted for this call. memory_use_outcomes records whether they helped. When something goes wrong (the user corrects, TL rejects, QA fails), a new memory is created, embedded, and indexed in real time. The next iteration of this exact loop benefits from that new lesson.\n\
             \n\
             HOW INFO TRANSFORMS toward the user goal: ambient context (rows in tables) → semantically relevant subset (vector search) → formatted prompt section → AI output (informed by past mistakes) → outcome telemetry → new memory → improved next pass. The loop is the self-learning substrate the slogan claims. Run system.memory.list / system.memory.similar to inspect what the system currently remembers.",
    })));

    // ── Row 1: forward path ────────────────────────────────────────────
    const ROW1_Y: i64 = 360;

    let forward: [Step; 7] = [
        Step { label: "TRIGGER\nuser turn or skill\ndecides it needs context", fill: C_TRIGGER,
            skill: "memory-rag-context\nor system.memory.similar", tables: "—" },
        Step { label: "EMBED QUERY\nencode user text\n→ vector(384)", fill: C_EMBED,
            skill: "embed.mjs\n(transformers.js BGE)", tables: "—" },
        Step { label: "VECTOR SEARCH\ncosine similarity\nminScore filter", fill: C_SEARCH,
            skill: "catalog-find", tables: "memory.catalog_embeddings" },
        Step { label: "TAGS FILTER\ntype, validated,\nsuperseded_by IS NULL", fill: C_FILTER,
            skill: "(SQL filter)", tables: "memory.memories" },
        Step { label: "LINKS BFS\nexpand via\nrelates_to / supersedes", fill: C_FETCH,
            skill: "memory-graph BFS", tables: "memory.memory_links" },
        Step { label: "FORMAT BLOCK\nrender memories as\nprompt context section", fill: C_FORMAT,
            skill: "memory-rag-context\n(formatter)", tables: "—" },
        Step { label: "AI CONSUMES\nblock injected into\nprompt_template input_kv", fill: C_AI,
            skill: "ai-call", tables: "brain.ai_calls\nbrain.ai_call_outputs" },
    ];

    struct Center { left: i64, right: i64, mid_y: i64 }

    let mut x = LEFT;
    let mut centers: Vec<Center> = Vec::new();
    for step in &forward {
        extend_els(&mut e, crate::render_excalidraw_emit::rect(&json!({
            "x": x, "y": ROW1_Y, "width": BOX_W, "height": BOX_H,
            "label": step.label, "fill": step.fill, "fontSize": 11,
        })));
        extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
            "x": x + 8, "y": ROW1_Y + BOX_H + 6,
            "text": step.skill, "fontSize": 9, "color": "#1565c0",
        })));
        extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
            "x": x + 8, "y": ROW1_Y + BOX_H + 22,
            "text": format!("tables: {}", step.tables), "fontSize": 9, "color": "#6e7681",
        })));
        centers.push(Center { left: x, right: x + BOX_W, mid_y: ROW1_Y + BOX_H / 2 });
        x += BOX_W + GAP;
    }
    for i in 0..centers.len().saturating_sub(1) {
        extend_els(&mut e, crate::render_excalidraw_emit::arrow(&json!({
            "x1": centers[i].right, "y1": centers[i].mid_y,
            "x2": centers[i + 1].left, "y2": centers[i + 1].mid_y,
            "stroke": "#7b1fa2",
        })));
    }

    // ── Row 2: feedback path ────────────────────────────────────────────
    const ROW2_Y: i64 = ROW1_Y + 240;

    let back: [Step; 5] = [
        Step { label: "AI RESPONSE\nplus output_kv\n(structured)", fill: C_AI,
            skill: "(ai-call return)", tables: "—" },
        Step { label: "LOG WHICH MEMORIES\nwere actually used\nfor THIS call", fill: C_LOG,
            skill: "memory-use-record", tables: "memory.memory_uses" },
        Step { label: "OUTCOME RECORDED\nwas it useful? did it\ncause a correction?", fill: C_OUTCOME,
            skill: "memory-use-outcome", tables: "memory.memory_use_outcomes" },
        Step { label: "LEARN MOMENT\nrejection / correction\n→ learnInsight", fill: C_LEARN,
            skill: "system.learn.insight", tables: "memory.memories (NEW)" },
        Step { label: "NEW MEMORY\nembedded + indexed\nnext cycle picks it up", fill: C_EMBED,
            skill: "memory-add + embed", tables: "memory.memories\nmemory.catalog_embeddings" },
    ];

    let mut bx = LEFT + 5 * (BOX_W + GAP);
    let mut back_centers: Vec<Center> = Vec::new();
    for step in &back {
        extend_els(&mut e, crate::render_excalidraw_emit::rect(&json!({
            "x": bx, "y": ROW2_Y, "width": BOX_W, "height": BOX_H,
            "label": step.label, "fill": step.fill, "fontSize": 11,
        })));
        extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
            "x": bx + 8, "y": ROW2_Y + BOX_H + 6,
            "text": step.skill, "fontSize": 9, "color": "#1565c0",
        })));
        extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
            "x": bx + 8, "y": ROW2_Y + BOX_H + 22,
            "text": format!("tables: {}", step.tables), "fontSize": 9, "color": "#6e7681",
        })));
        back_centers.push(Center { left: bx, right: bx + BOX_W, mid_y: ROW2_Y + BOX_H / 2 });
        bx -= BOX_W + GAP;
    }
    for i in 0..back_centers.len().saturating_sub(1) {
        extend_els(&mut e, crate::render_excalidraw_emit::arrow(&json!({
            "x1": back_centers[i].left, "y1": back_centers[i].mid_y,
            "x2": back_centers[i + 1].right, "y2": back_centers[i + 1].mid_y,
            "stroke": "#388e3c",
        })));
    }

    // Vertical connector: row 1 end (AI consumes) → row 2 start (AI response)
    let ai_box_center = &centers[centers.len() - 1];
    extend_els(&mut e, crate::render_excalidraw_emit::arrow(&json!({
        "x1": ai_box_center.right - BOX_W / 2, "y1": ROW1_Y + BOX_H,
        "x2": back_centers[0].right - BOX_W / 2, "y2": ROW2_Y,
        "stroke": "#7b1fa2",
    })));

    // Loop-back: last back step (new memory) → first forward step (trigger)
    let last_back = &back_centers[back_centers.len() - 1];
    let first_forward = &centers[0];
    extend_els(&mut e, crate::render_excalidraw_emit::arrow(&json!({
        "x1": last_back.left + BOX_W / 2, "y1": ROW2_Y + BOX_H + 50,
        "x2": last_back.left + BOX_W / 2, "y2": ROW2_Y + BOX_H + 110,
        "stroke": C_LOOP,
    })));
    extend_els(&mut e, crate::render_excalidraw_emit::arrow(&json!({
        "x1": last_back.left + BOX_W / 2, "y1": ROW2_Y + BOX_H + 110,
        "x2": first_forward.left + BOX_W / 2, "y2": ROW2_Y + BOX_H + 110,
        "label": "next RAG cycle inherits",
        "stroke": C_LOOP,
    })));
    extend_els(&mut e, crate::render_excalidraw_emit::arrow(&json!({
        "x1": first_forward.left + BOX_W / 2, "y1": ROW2_Y + BOX_H + 110,
        "x2": first_forward.left + BOX_W / 2, "y2": ROW1_Y + BOX_H,
        "stroke": C_LOOP,
    })));

    // ── Side legend ──────────────────────────────────────────────────────
    const LEG_X: i64 = 40;
    let leg_y: i64 = ROW2_Y + BOX_H + 170;
    extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
        "x": LEG_X, "y": leg_y, "text": "memory types stored", "fontSize": 14, "color": "#1e1e1e",
    })));

    let types: [(&str, &str, &str); 5] = [
        ("correction", "#ffcdd2", "TL rejected, QA failed, user corrected → naive vs corrected captured"),
        ("preference", "#bbdefb", "user-stated style/approach preferences (validated=1)"),
        ("decision", "#c8e6c9", "architectural / scope decisions worth replaying"),
        ("lesson", "#fff9c4", "general insights extracted from prior runs"),
        ("observation", "#e1bee7", "raw note (low-confidence; pre-validation)"),
    ];
    let mut ly = leg_y + 26;
    for (t, c, desc) in &types {
        extend_els(&mut e, crate::render_excalidraw_emit::rect(&json!({
            "x": LEG_X, "y": ly, "width": 22, "height": 22, "fill": c,
        })));
        extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
            "x": LEG_X + 32, "y": ly + 3, "text": format!("{t} — {desc}"), "fontSize": 11, "color": "#1e1e1e",
        })));
        ly += 28;
    }

    Ok(crate::render_excalidraw_emit::buildExcalidraw(
        &json!(e),
        &json!({ "name": "antiloki-rag-flow-2026-01-01" }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(db: &Connection) {
        db.execute_batch(
            "ATTACH DATABASE ':memory:' AS memory;
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, type TEXT);
             CREATE TABLE memory.memory_links (id INTEGER PRIMARY KEY);
             CREATE TABLE memory.embeddings (id INTEGER PRIMARY KEY);
             CREATE TABLE memory.catalog_embeddings (id INTEGER PRIMARY KEY);
             CREATE TABLE memory.memory_uses (id INTEGER PRIMARY KEY);
             CREATE TABLE memory.memory_use_outcomes (id INTEGER PRIMARY KEY);"
        ).unwrap();
    }

    #[test]
    fn empty_state_counts_are_zero_and_shape_is_complete() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        let out = excalidrawRag(&json!({}), &db).unwrap();
        assert_eq!(out["appState"]["name"], json!("antiloki-rag-flow-2026-01-01"));
        let els = out["elements"].as_array().unwrap();
        assert!(els.iter().any(|el| el.get("text").and_then(Value::as_str)
            == Some("0 memories (0 corrections) · 0 links · 0+0 vectors · 0 uses · 0 outcomes")));
        assert!(els.iter().any(|el| el.get("text").and_then(Value::as_str)
            .map_or(false, |t| t.starts_with("WHAT THIS DIAGRAM SHOWS"))));
        assert!(els.iter().any(|el| el.get("text").and_then(Value::as_str)
            == Some("memory types stored")));
    }

    #[test]
    fn counts_reflect_row_counts() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        db.execute_batch(
            "INSERT INTO memory.memories (type) VALUES ('correction'),('lesson'),('decision');
             INSERT INTO memory.memory_links VALUES (1),(2);
             INSERT INTO memory.embeddings VALUES (1);
             INSERT INTO memory.catalog_embeddings VALUES (1),(2),(3);
             INSERT INTO memory.memory_uses VALUES (1);
             INSERT INTO memory.memory_use_outcomes VALUES (1),(2);"
        ).unwrap();
        let out = excalidrawRag(&json!({}), &db).unwrap();
        let els = out["elements"].as_array().unwrap();
        assert!(els.iter().any(|el| el.get("text").and_then(Value::as_str)
            == Some("3 memories (1 corrections) · 2 links · 1+3 vectors · 1 uses · 2 outcomes")));
    }

    #[test]
    fn missing_tables_are_treated_as_zero_count() {
        let db = Connection::open_in_memory().unwrap();
        let out = excalidrawRag(&json!({}), &db).unwrap();
        let els = out["elements"].as_array().unwrap();
        assert!(els.iter().any(|el| el.get("text").and_then(Value::as_str)
            == Some("0 memories (0 corrections) · 0 links · 0+0 vectors · 0 uses · 0 outcomes")));
    }

    #[test]
    fn arrows_rects_and_legend_swatches_present() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        let out = excalidrawRag(&json!({}), &db).unwrap();
        let els = out["elements"].as_array().unwrap();
        let rect_count = els.iter().filter(|el| el["type"] == json!("rectangle")).count();
        // 1 narrative panel + 7 forward boxes + 5 back boxes + 5 legend swatches = 18
        assert_eq!(rect_count, 18);
        let arrow_count = els.iter().filter(|el| el["type"] == json!("arrow")).count();
        // 6 forward + 4 back + 1 connector + 3 loop-back = 14
        assert_eq!(arrow_count, 14);
    }
}
