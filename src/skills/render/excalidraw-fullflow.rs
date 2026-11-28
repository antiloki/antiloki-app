//! excalidraw-fullflow.rs — twin of skills/render/excalidraw-fullflow.mjs (db READ lane, single
//! default export `excalidrawFullflow`). Same class as excalidraw-board.rs / excalidraw-emit.rs's
//! non-`buildExcalidraw` exports: this function's ENTIRE output is assembled by calling
//! `render_excalidraw_emit::rect`/`ellipse`/`diamond`/`text`/`arrow`/`narrativeBlock`, each of
//! which unconditionally calls the module-private OS-CSPRNG `id()` on every element on every
//! success path. A real id can never be byte-identical to Node's `crypto.randomBytes` across the
//! two lanes, so this unit is BESPOKE NATIVE: real, correct code, proven only by `#[cfg(test)]`
//! (checking shape/layout/stat wiring, not exact ids), no conform-db fixture / frontier entry /
//! dispatch arm (see excalidraw-board.rs's identical precedent + the crossing manifest for this
//! module).
//!
//! Frozen clock (playbook): `new Date().toISOString().slice(0, 10)` → the literal `"2026-01-01"`.
//! `safeCount` mirrors the JS try/catch-swallow-to-0 semantics.

use rusqlite::Connection;
use serde_json::{json, Value};

fn extend_els(out: &mut Vec<Value>, v: Value) {
    if let Value::Array(els) = v {
        out.extend(els);
    }
}

fn safe_count(db: &Connection, sql: &str) -> i64 {
    match crate::db_json::query_rows(db, sql, &[]) {
        Ok(Value::Array(rows)) => rows
            .first()
            .and_then(|r| r.get("n"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        _ => 0,
    }
}

/// Vertical arrow from (cx, y1) to (cx, y2) with optional label.
fn arrow_v(cx: i64, y1: i64, y2: i64, label: Option<&str>, stroke: &str) -> Value {
    let mut opts = json!({ "x1": cx, "y1": y1, "x2": cx, "y2": y2, "stroke": stroke });
    if let Some(l) = label {
        opts["label"] = json!(l);
    }
    crate::render_excalidraw_emit::arrow(&opts)
}

fn box_(x: i64, y: i64, w: i64, h: i64, label: &str, fill: &str, font_size: i64) -> Value {
    crate::render_excalidraw_emit::rect(&json!({
        "x": x, "y": y, "width": w, "height": h, "label": label, "fill": fill, "fontSize": font_size
    }))
}

/// `export default function excalidrawFullflow(input, context)`
pub fn excalidrawFullflow(_input: &Value, db: &Connection) -> Result<Value, String> {
    let actions = safe_count(db, "SELECT COUNT(*) AS n FROM actions");
    let skills = safe_count(db, "SELECT COUNT(*) AS n FROM memory.skills");
    let agents = safe_count(db, "SELECT COUNT(*) AS n FROM agents");
    let laws = safe_count(db, "SELECT COUNT(*) AS n FROM laws.laws");
    let providers = safe_count(db, "SELECT COUNT(*) AS n FROM brain.ai_providers WHERE active=1");
    let memories = safe_count(db, "SELECT COUNT(*) AS n FROM memory.memories");

    let c_user = "#bbdefb";
    let c_fe = "#e3f2fd";
    let c_be = "#e1f5fe";
    let c_gate = "#fff9c4";
    let c_block = "#ffcdd2";
    let c_compose = "#e8f5e9";
    let c_ai = "#f3e5f5";
    let c_memory = "#fce4ec";
    let c_audit = "#fff3e0";
    let c_db = "#eceff1";
    let arrow_main = "#1565c0";
    let arrow_block = "#c62828";
    let arrow_ai = "#7b1fa2";
    let arrow_audit = "#ef6c00";
    let arrow_db = "#546e7a";

    let mut e: Vec<Value> = Vec::new();

    // ── HEADER ──────────────────────────────────────────────────────────
    extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
        "x": 40, "y": 30,
        "text": "antiloki · full flow · user input → output (complete substrate trace)",
        "fontSize": 24, "color": arrow_main,
    })));
    extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
        "x": 40, "y": 64,
        "text": format!(
            "{actions} actions · {skills} skills · {agents} agents · {laws} laws · {providers} active providers · {memories} memories · 5 DBs"
        ),
        "fontSize": 12, "color": "#6e7681",
    })));
    extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
        "x": 40, "y": 84,
        "text": "every arrow below is a real code path. every ledger write happens. nothing here is aspirational.",
        "fontSize": 11, "color": "#9e9e9e",
    })));

    // ── NARRATIVE INTRO ─────────────────────────────────────────────────
    extend_els(&mut e, crate::render_excalidraw_emit::narrativeBlock(&json!({
        "x": 40, "y": 100, "width": 1900,
        "text":
            "WHAT THIS DIAGRAM SHOWS — the complete journey of a single thought from your fingertips to a result.\n\
             \n\
             You type a message in the chat. The FE packages it as a turn and posts it to the BE. The BE invokes action-run — the only orchestrator that may dispatch work. Before doing anything, action-run consults the law table: if a law refuses (e.g., the action did not come from the driver loop, or you tried to bypass with-audit), the dispatch is denied, the attempt is recorded in law_violations, and the substrate refuses to act. If allowed, the action's steps_json composes one or more skills in order. Each skill may call an LLM (through ai-call, tier-routed across providers), inject context from memory (the RAG branch, right side), and write audited mutations through with-audit (which chains every change into sacred_timeline so tampering breaks the SHA chain). Five SQLite databases participate in one atomic transaction via ATTACH DATABASE. When the work returns, the runtime_events row gets its outcome; the BE bundles a TurnResponse; the FE renders it via PresentationView, branching on view_intent (board, table, excalidraw, code, etc.). Your original goal — the interviewer_turn that started this — is threaded through every record via user_goal_id, so later you can ask \"show me everything that happened because of message #42\" and the substrate answers in one SQL query.\n\
             \n\
             HOW INFO TRANSFORMS: natural language → classified intent → action name → composed skill chain → AI calls (with memory injected) → DB mutations (audited) → typed result → rendered view. Each transformation is observable in a ledger. The substrate makes no claim it cannot prove from data.",
    })));

    let y_offset: i64 = 240;

    let spine_x: i64 = 80;
    let ai_x: i64 = 880;
    let mem_x: i64 = 1170;
    let audit_x: i64 = 1460;
    let db_x: i64 = 1750;

    let box_w: i64 = 320;
    let box_h: i64 = 70;

    let mut y: i64 = 130 + y_offset;
    let step: i64 = 110;

    // 1. USER input
    extend_els(&mut e, box_(spine_x, y, box_w, box_h, "USER\nTypes message in chat", c_user, 14));
    let user_in_y = y + box_h;
    y += step;

    // 2. FE
    extend_els(&mut e, box_(spine_x, y, box_w, box_h, "FE (React + Vite)\nInputBox → /api/turn (REST)", c_fe, 13));
    extend_els(&mut e, arrow_v(spine_x + box_w / 2, user_in_y, y, Some("click send"), arrow_main));
    let fe_y = y + box_h;
    y += step;

    // 3. BE
    extend_els(&mut e, box_(spine_x, y, box_w, box_h, "BE (NestJS) controller\nEngineService.dispatch(action, input, actor, source='chat')", c_be, 12));
    extend_els(&mut e, arrow_v(spine_x + box_w / 2, fe_y, y, None, arrow_main));
    let be_y = y + box_h;
    y += step;

    // 4. action-run (diamond)
    let ar_x = spine_x + 20;
    let ar_y = y;
    let ar_w = box_w - 40;
    let ar_h: i64 = 90;
    extend_els(&mut e, crate::render_excalidraw_emit::diamond(&json!({
        "x": ar_x, "y": ar_y, "width": ar_w, "height": ar_h,
        "label": "action-run (the only orchestrator)\nthreads context.source, user_goal_id",
        "fontSize": 11, "fill": "#fff8e1",
    })));
    extend_els(&mut e, arrow_v(spine_x + box_w / 2, be_y, ar_y, None, arrow_main));
    let ar_bottom_y = ar_y + ar_h;
    y = ar_bottom_y + 30;

    // ── LAW GATE ─────────────────────────────────────────────────────────
    let law_x = spine_x + box_w + 60;
    let law_y = ar_y - 10;
    extend_els(&mut e, box_(
        law_x, law_y, 320, 110,
        &format!(
            "LAW-CHECK gate ({laws} laws)\n• LONE_SKILL_DISPATCH_FORBIDDEN (driver gate)\n• COMMIT_CODE_BEFORE_AUDIT_COMMIT\n• NEVER_BYPASS_WITH_AUDIT\n• NEVER_FALSIFY_PROTOCOL_STATE"
        ),
        c_gate, 10,
    ));
    extend_els(&mut e, crate::render_excalidraw_emit::arrow(&json!({
        "x1": ar_x + ar_w, "y1": ar_y + ar_h / 2, "x2": law_x, "y2": law_y + 55, "label": "consult"
    })));

    // BLOCK branch
    extend_els(&mut e, box_(
        law_x + 360, law_y + 30, 250, 70,
        "BLOCKED\nthrow PERMISSION_DENIED\nlaw_violations row\nactor never sees mutation",
        c_block, 10,
    ));
    extend_els(&mut e, crate::render_excalidraw_emit::arrow(&json!({
        "x1": law_x + 320, "y1": law_y + 55, "x2": law_x + 360, "y2": law_y + 65,
        "label": "deny", "stroke": arrow_block,
    })));

    // ── ACTION CATALOG ───────────────────────────────────────────────────
    let catalog_y = y;
    extend_els(&mut e, box_(
        spine_x, catalog_y, box_w, box_h,
        &format!("actions table · {actions} canonical\nagent_actions table · grants (RBAC)"),
        c_compose, 12,
    ));
    extend_els(&mut e, arrow_v(spine_x + box_w / 2, ar_bottom_y, catalog_y, Some("allowed"), arrow_main));
    let catalog_bottom = catalog_y + box_h;
    y += step;

    // ── COMPOSE ───────────────────────────────────────────────────────────
    let compose_y = y;
    extend_els(&mut e, box_(
        spine_x, compose_y, box_w, box_h,
        "compose steps_json (skill chain)\nresolves $.input.X / $.results.X paths",
        c_compose, 12,
    ));
    extend_els(&mut e, arrow_v(spine_x + box_w / 2, catalog_bottom, compose_y, None, arrow_main));
    let compose_bottom = compose_y + box_h;
    y += step;

    // ── SKILL EXECUTION ──────────────────────────────────────────────────
    let skill_y = y;
    extend_els(&mut e, box_(
        spine_x, skill_y, box_w, box_h,
        "skill #1 (e.g., dev-implement-code)\nimport + invoke .mjs default export",
        c_compose, 12,
    ));
    extend_els(&mut e, arrow_v(spine_x + box_w / 2, compose_bottom, skill_y, None, arrow_main));
    let skill_center_y = skill_y + box_h / 2;
    let skill_bottom = skill_y + box_h;

    // ── AI COLUMN ─────────────────────────────────────────────────────────
    let mut ai_y = skill_y - 50;
    extend_els(&mut e, box_(ai_x, ai_y, box_w, box_h, "ai-call (single chokepoint)\nprompt_templates lookup + cache check", c_ai, 11));
    extend_els(&mut e, crate::render_excalidraw_emit::arrow(&json!({
        "x1": spine_x + box_w, "y1": skill_center_y, "x2": ai_x, "y2": ai_y + box_h / 2,
        "label": "call LLM", "stroke": arrow_ai,
    })));
    ai_y += step;
    extend_els(&mut e, box_(ai_x, ai_y, box_w, box_h, &format!("ai_providers (tier-routed)\n{providers} active · adapter_module"), c_ai, 11));
    extend_els(&mut e, arrow_v(ai_x + box_w / 2, ai_y - step + box_h, ai_y, Some(""), arrow_ai));
    ai_y += step;
    extend_els(&mut e, crate::render_excalidraw_emit::ellipse(&json!({
        "x": ai_x + 50, "y": ai_y, "width": box_w - 100, "height": box_h,
        "label": "external LLM\n(claude / qwen / deepseek / openai)", "fill": "#fff", "fontSize": 11,
    })));
    extend_els(&mut e, arrow_v(ai_x + box_w / 2, ai_y - step + box_h, ai_y, Some(""), arrow_ai));
    ai_y += step;
    extend_els(&mut e, box_(ai_x, ai_y, box_w, box_h, "validate schema (prompt_output_fields)\npersist ai_calls + ai_call_outputs", c_ai, 11));
    extend_els(&mut e, arrow_v(ai_x + box_w / 2, ai_y - step + box_h, ai_y, Some(""), arrow_ai));
    extend_els(&mut e, crate::render_excalidraw_emit::arrow(&json!({
        "x1": ai_x, "y1": ai_y + box_h / 2, "x2": spine_x + box_w, "y2": skill_center_y + 30,
        "label": "output_kv", "stroke": arrow_ai,
    })));

    // ── MEMORY / RAG COLUMN ─────────────────────────────────────────────
    let mut mem_y = skill_y - 50;
    extend_els(&mut e, crate::render_excalidraw_emit::rect(&json!({
        "x": mem_x, "y": mem_y, "width": 270, "height": box_h,
        "label": "memory-rag-context\nembeddings sim + tags filter", "fill": c_memory, "fontSize": 11,
    })));
    extend_els(&mut e, crate::render_excalidraw_emit::arrow(&json!({
        "x1": spine_x + box_w, "y1": skill_center_y - 8, "x2": mem_x, "y2": mem_y + box_h / 2,
        "label": "rag inject", "stroke": "#c2185b",
    })));
    mem_y += step;
    extend_els(&mut e, crate::render_excalidraw_emit::rect(&json!({
        "x": mem_x, "y": mem_y, "width": 270, "height": box_h,
        "label": format!("memory.memories\n{memories} rows (correction/lesson/decision)"),
        "fill": c_memory, "fontSize": 11,
    })));
    extend_els(&mut e, arrow_v(mem_x + 135, mem_y - step + box_h, mem_y, Some(""), "#c2185b"));
    mem_y += step;
    extend_els(&mut e, crate::render_excalidraw_emit::rect(&json!({
        "x": mem_x, "y": mem_y, "width": 270, "height": box_h,
        "label": "memory.embeddings\n+ memory_links graph (BFS)", "fill": c_memory, "fontSize": 11,
    })));
    extend_els(&mut e, arrow_v(mem_x + 135, mem_y - step + box_h, mem_y, Some(""), "#c2185b"));
    mem_y += step;
    extend_els(&mut e, crate::render_excalidraw_emit::rect(&json!({
        "x": mem_x, "y": mem_y, "width": 270, "height": box_h,
        "label": "memory_uses / memory_use_outcomes\n(learning loop telemetry)", "fill": c_memory, "fontSize": 11,
    })));
    extend_els(&mut e, arrow_v(mem_x + 135, mem_y - step + box_h, mem_y, Some(""), "#c2185b"));

    // ── AUDIT COLUMN ──────────────────────────────────────────────────────
    let mut aud_y = skill_y - 50;
    extend_els(&mut e, crate::render_excalidraw_emit::rect(&json!({
        "x": audit_x, "y": aud_y, "width": 270, "height": box_h,
        "label": "with-audit wrapper\nreads BEFORE state", "fill": c_audit, "fontSize": 11,
    })));
    extend_els(&mut e, crate::render_excalidraw_emit::arrow(&json!({
        "x1": spine_x + box_w, "y1": skill_center_y + 16, "x2": audit_x, "y2": aud_y + box_h / 2,
        "label": "on every mutation", "stroke": arrow_audit,
    })));
    aud_y += step;
    extend_els(&mut e, crate::render_excalidraw_emit::rect(&json!({
        "x": audit_x, "y": aud_y, "width": 270, "height": box_h,
        "label": "runs the wrapped skill\nreads AFTER state", "fill": c_audit, "fontSize": 11,
    })));
    extend_els(&mut e, arrow_v(audit_x + 135, aud_y - step + box_h, aud_y, Some(""), arrow_audit));
    aud_y += step;
    extend_els(&mut e, crate::render_excalidraw_emit::rect(&json!({
        "x": audit_x, "y": aud_y, "width": 270, "height": box_h,
        "label": "compute digital_print\n= SHA256(prev | ts | actor | op | before | after | payload)",
        "fill": c_audit, "fontSize": 10,
    })));
    extend_els(&mut e, arrow_v(audit_x + 135, aud_y - step + box_h, aud_y, Some(""), arrow_audit));
    aud_y += step;
    extend_els(&mut e, crate::render_excalidraw_emit::rect(&json!({
        "x": audit_x, "y": aud_y, "width": 270, "height": box_h,
        "label": "INSERT sacred_timeline\nIMMUTABLE (triggers reject UPDATE+DELETE)",
        "fill": c_audit, "fontSize": 10,
    })));
    extend_els(&mut e, arrow_v(audit_x + 135, aud_y - step + box_h, aud_y, Some(""), arrow_audit));
    let audit_bottom = aud_y + box_h;

    // Phase event side-trail
    extend_els(&mut e, crate::render_excalidraw_emit::rect(&json!({
        "x": audit_x, "y": aud_y + step, "width": 270, "height": box_h,
        "label": "card_phase_events (auto-fire)\npl_draft / tl_sign / dev_impl / qa_pass / close",
        "fill": c_audit, "fontSize": 10,
    })));
    extend_els(&mut e, arrow_v(audit_x + 135, audit_bottom, aud_y + step, Some(""), arrow_audit));

    // ── 5 DATABASES COLUMN ────────────────────────────────────────────────
    let dbs: [(&str, String); 5] = [
        ("state.sqlite", "cards, contracts, agents,\nactions, sacred_timeline,\nphase_events, sprints".to_string()),
        ("memory.sqlite", "memories, embeddings,\nlinks, anatomy snapshots".to_string()),
        ("brain.sqlite", "ai_calls, providers,\nprompt_templates, driver_ticks".to_string()),
        ("laws.sqlite", format!("laws ({laws}), clauses,\nlaw_violations")),
        ("knowledge.sqlite", "documents, extracts,\ndocument_links".to_string()),
    ];
    for (i, (name, desc)) in dbs.iter().enumerate() {
        let dy = skill_y - 50 + (i as i64) * (box_h + 20);
        extend_els(&mut e, crate::render_excalidraw_emit::ellipse(&json!({
            "x": db_x, "y": dy, "width": 220, "height": box_h,
            "label": format!("{name}\n{desc}"), "fill": c_db, "fontSize": 9,
        })));
    }
    extend_els(&mut e, crate::render_excalidraw_emit::arrow(&json!({
        "x1": audit_x + 270, "y1": skill_center_y + 80, "x2": db_x, "y2": skill_center_y + 80,
        "label": "ATTACH DATABASE (one transaction)", "stroke": arrow_db,
    })));

    // ── Continue spine: return path ────────────────────────────────────────
    y = skill_bottom + 100;

    // 5. runtime_events
    let rt_y = y;
    extend_els(&mut e, box_(
        spine_x, rt_y, box_w, box_h,
        "update runtime_events row\noutcome ∈ {success, error, blocked, skipped}\n+ user_goal_id, source persisted",
        c_audit, 11,
    ));
    extend_els(&mut e, arrow_v(spine_x + box_w / 2, skill_bottom, rt_y, Some("return"), arrow_main));
    let rt_bottom = rt_y + box_h;
    y += step;

    // 6. BE response → FE
    let be_resp_y = y;
    extend_els(&mut e, box_(
        spine_x, be_resp_y, box_w, box_h,
        "BE → FE (JSON over REST)\nTurnResponse {presentation_md, view_intent, view_data,\ndispatch_results, context_bundle, ai_telemetry}",
        c_be, 10,
    ));
    extend_els(&mut e, arrow_v(spine_x + box_w / 2, rt_bottom, be_resp_y, None, arrow_main));
    let be_resp_bottom = be_resp_y + box_h;
    y += step;

    // 7. FE PresentationView
    let pv_y = y;
    extend_els(&mut e, box_(
        spine_x, pv_y, box_w, box_h + 30,
        "FE PresentationView\nview_intent ∈ {inline, board, card_detail, table,\ncode, excalidraw, none}",
        c_fe, 10,
    ));
    extend_els(&mut e, arrow_v(spine_x + box_w / 2, be_resp_bottom, pv_y, None, arrow_main));
    let pv_bottom = pv_y + box_h + 30;
    y += step + 30;

    // 8. USER sees output
    let user_out_y = y;
    extend_els(&mut e, box_(
        spine_x, user_out_y, box_w, box_h,
        "USER\nReads response · 'reasoning' panel exposes full trace:\nai calls + provider + cost + memories injected + audit rows",
        c_user, 10,
    ));
    extend_els(&mut e, arrow_v(spine_x + box_w / 2, pv_bottom, user_out_y, None, arrow_main));

    // ── Side-band: every layer logged ──────────────────────────────────────
    let side_x = ai_x;
    let side_y = rt_y;
    extend_els(&mut e, box_(
        side_x, side_y, 280, 200,
        "EVERYTHING IS RECEIPTED:\n\
         • runtime_events: every dispatch (source, goal_id)\n\
         • sacred_timeline: every audited mutation (SHA chain)\n\
         • ai_calls + outputs: every LLM call\n\
         • law_violations: every block/warn/exception\n\
         • card_phase_events: every lifecycle step\n\
         • timeline_commits: maps audit → git SHA\n\
         \n\
         → man.audit.truth.cross-check verifies them all agree",
        "#f1f8e9", 10,
    ));

    Ok(crate::render_excalidraw_emit::buildExcalidraw(
        &json!(e),
        &json!({ "name": "antiloki-fullflow-2026-01-01" }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(db: &Connection) {
        db.execute_batch(
            "CREATE TABLE actions (id INTEGER PRIMARY KEY);
             ATTACH DATABASE ':memory:' AS memory;
             CREATE TABLE memory.skills (id INTEGER PRIMARY KEY);
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY);
             CREATE TABLE agents (id INTEGER PRIMARY KEY);
             ATTACH DATABASE ':memory:' AS laws;
             CREATE TABLE laws.laws (id INTEGER PRIMARY KEY);
             ATTACH DATABASE ':memory:' AS brain;
             CREATE TABLE brain.ai_providers (id INTEGER PRIMARY KEY, active INTEGER);",
        )
        .unwrap();
    }

    #[test]
    fn stats_are_pulled_live_and_appear_in_header_text() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        db.execute("INSERT INTO actions DEFAULT VALUES", []).unwrap();
        db.execute("INSERT INTO actions DEFAULT VALUES", []).unwrap();
        db.execute("INSERT INTO laws.laws DEFAULT VALUES", []).unwrap();
        db.execute("INSERT INTO brain.ai_providers (active) VALUES (1)", []).unwrap();
        db.execute("INSERT INTO brain.ai_providers (active) VALUES (0)", []).unwrap();

        let out = excalidrawFullflow(&json!({}), &db).unwrap();
        assert_eq!(out["type"], json!("excalidraw"));
        assert_eq!(out["appState"]["name"], json!("antiloki-fullflow-2026-01-01"));
        let els = out["elements"].as_array().unwrap();
        assert!(els.iter().any(|el| el.get("text").and_then(Value::as_str)
            .map_or(false, |t| t.starts_with("2 actions · 0 skills · 0 agents · 1 laws · 1 active providers · 0 memories · 5 DBs"))));
    }

    #[test]
    fn safe_count_swallows_missing_table_to_zero() {
        let db = Connection::open_in_memory().unwrap();
        // no tables created at all — every safe_count query fails and yields 0.
        let out = excalidrawFullflow(&json!({}), &db).unwrap();
        let els = out["elements"].as_array().unwrap();
        assert!(els.iter().any(|el| el.get("text").and_then(Value::as_str)
            .map_or(false, |t| t.starts_with("0 actions · 0 skills · 0 agents · 0 laws · 0 active providers · 0 memories · 5 DBs"))));
    }

    #[test]
    fn output_contains_law_gate_block_branch_and_all_five_dbs() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        let out = excalidrawFullflow(&json!({}), &db).unwrap();
        let els = out["elements"].as_array().unwrap();
        assert!(els.iter().any(|el| el.get("text").and_then(Value::as_str).map_or(false, |t| t.starts_with("LAW-CHECK gate"))));
        assert!(els.iter().any(|el| el.get("text").and_then(Value::as_str) == Some("BLOCKED\nthrow PERMISSION_DENIED\nlaw_violations row\nactor never sees mutation")));
        for name in ["state.sqlite", "memory.sqlite", "brain.sqlite", "laws.sqlite", "knowledge.sqlite"] {
            assert!(els.iter().any(|el| el.get("text").and_then(Value::as_str).map_or(false, |t| t.starts_with(name))));
        }
    }
}
