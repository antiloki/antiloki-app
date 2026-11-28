//! excalidraw-lifecycle.rs — twin of skills/render/excalidraw-lifecycle.mjs (db READ lane, single
//! default export `excalidrawLifecycle`). Same class as excalidraw-board.rs / excalidraw-emit.rs's
//! non-`buildExcalidraw` exports: this function's ENTIRE output is built by calling
//! `render_excalidraw_emit::rect`/`text`/`arrow`/`narrativeBlock`, each of which unconditionally
//! calls the module-private OS-CSPRNG `id()` on every element on every success path. A real id can
//! never be byte-identical to Node's `crypto.randomBytes` across the two lanes, so this unit is
//! BESPOKE NATIVE: real, correct code, proven only by `#[cfg(test)]` (checking shape/lane
//! structure, not exact ids), no conform-db fixture / frontier entry / dispatch arm (see
//! excalidraw-board.rs's identical precedent + the crossing manifest for this module).
//!
//! Frozen clock (playbook): `new Date().toISOString().slice(0, 10)` → the literal `"2026-01-01"`.
//! `db.laws.laws` is a cross-db (ATTACH) read, wrapped the same as the JS `safeCount` — any query
//! error (e.g. `laws` not attached) is swallowed and counts as 0, exactly like the JS try/catch.

use rusqlite::Connection;
use serde_json::{json, Value};

struct Lane {
    agent: &'static str,
    color: &'static str,
    title: &'static str,
    tools: &'static [(&'static str, &'static str)],
    reads: &'static str,
    writes: &'static str,
    gate_law: &'static str,
}

const C_DRIVER: &str = "#e3f2fd";
const C_GATE: &str = "#fff9c4";
const C_PL: &str = "#fff8e1";
const C_TL: &str = "#fce4ec";
const C_DEV: &str = "#e8f5e9";
const C_QA: &str = "#e1f5fe";
const C_CLOSE: &str = "#f3e5f5";
const C_ACTION: &str = "#ffffff";
const C_LEDGER: &str = "#ffe0b2";
const C_ARROW: &str = "#1565c0";
const C_ARROW_AUDIT: &str = "#ef6c00";

fn lanes() -> Vec<Lane> {
    vec![
        Lane {
            agent: "PL (atlas)", color: C_PL, title: "pl_draft",
            tools: &[
                ("pl.session.start", "open ideation session"),
                ("pl.session.advance.clarify", "ask user questions"),
                ("pl.session.advance.research", "pl.research (AI tier=mid)"),
                ("pl.session.advance.decompose", "split into N proposed cards"),
                ("pl.session.finalize", "build body_md + tier"),
                ("pl.session.approve", "with-audit(card-add)\n→ fires pl_draft"),
            ],
            reads: "memory.memories (RAG context)\nfile_sensitivity\nexisting cards",
            writes: "pl_sessions, pl_questions, pl_answers,\npl_decomposition_children, pl_proposed_cards,\ncards (new), sacred_timeline, card_phase_events",
            gate_law: "COMMIT_CODE_BEFORE_AUDIT_COMMIT",
        },
        Lane {
            agent: "TL (forge:arch)", color: C_TL, title: "tl_sign",
            tools: &[
                ("tl.pre-contract.data-query", "find conflicts with prior contracts"),
                ("tl.refine.microtasks", "smart-tier LLM → microtask plan\n(M/L size required, FEAT-TL-REFINE-MICROTASKS)"),
                ("contract.gate.tl.sign", "accept: persist contract,\nfire tl_sign · reject: write correction memory"),
                ("tl.sandbox.create", "optional: COW sandbox for DEV"),
            ],
            reads: "cards, prior contracts,\nlaws (must_obey, edge_cases)",
            writes: "contracts (microtasks_json),\nmemory.memories (corrections on reject),\nsacred_timeline, card_phase_events",
            gate_law: "NEVER_BYPASS_WITH_AUDIT",
        },
        Lane {
            agent: "DEV (forge:dev)", color: C_DEV, title: "dev_impl",
            tools: &[
                ("git-manager.contract.load", "read tl_approved contract"),
                ("dev-plan-impl-split", "topo-sort microtasks"),
                ("dev-exec-sandbox (per microtask)", "cheap-tier LLM in budget envelope"),
                ("dev-verify-chain", "deterministic patch verify\n+ optional AI sample"),
                ("git-manager.file.write (sandbox-aware)", "writes to COW sandbox path\n(substrate blocks out-of-scope!)"),
                ("git-manager.implement.code", "single-shot path → fires dev_impl"),
            ],
            reads: "contracts (microtasks_json, files_touched),\nbrain.ai_providers (tier-routed),\nmemory.memories (must_obey context)",
            writes: "sandbox files,\nbrain.ai_calls + ai_call_outputs,\nsacred_timeline (file ops),\ncard_phase_events",
            gate_law: "LONE_SKILL_DISPATCH_FORBIDDEN\n(action-only, no skill bypass)",
        },
        Lane {
            agent: "QA (warden)", color: C_QA, title: "qa_pass / qa_fail",
            tools: &[
                ("qa.review.run", "4 scanners: symbol/type-bypass/\nplaceholder/security · zero AI"),
                ("qa-acceptance-run", "check each contract.acceptance"),
                ("qa-gate-contract-verify", "sandbox vs contract scope drift"),
                ("qa-sandbox-promote", "tamper detection + atomic promote"),
                ("qa-sandbox-reset", "on failure: keep sandbox for retry"),
            ],
            reads: "sandbox files, contracts,\nfile_sensitivity (risk weights)",
            writes: "qa_runs, qa_run_results,\nmemory.memories (lessons on retry),\ncard_phase_events (qa_pass | qa_fail)",
            gate_law: "NEVER_FALSIFY_PROTOCOL_STATE\n(only real pass writes qa_pass)",
        },
        Lane {
            agent: "git-manager", color: C_CLOSE, title: "close",
            tools: &[
                ("git-manager.merge.gate", "require pl_draft + tl_sign +\ndev_impl + qa_pass all present"),
                ("git-manager.stage.validate", "staged ⊆ contract.files_touched"),
                ("git-manager.branch.init", "card-scoped branch"),
                ("git-manager.worktree.ops", "parallel work safety"),
                ("(commit + status=done +\nfires close)", "sacred_timeline card.status.change"),
            ],
            reads: "card_phase_events (all 4),\ncontracts.files_touched, git status",
            writes: "git commits, timeline_commits,\nsacred_timeline (card.status.change),\ncard_phase_events (close)",
            gate_law: "HISTORY_IS_SACRED_NO_REVERT",
        },
    ]
}

const LEDGERS: &[(&str, &str)] = &[
    ("sacred_timeline", "SHA-chained audited mutations\n(triggers reject UPDATE/DELETE)"),
    ("runtime_events", "every action + skill dispatch\n(source, user_goal_id, outcome)"),
    ("card_phase_events", "pl_draft / tl_sign / dev_impl /\nqa_pass / qa_fail / close"),
    ("law_violations", "every block / warn / exception\nwith law_id + context"),
    ("ai_calls + outputs", "every LLM invocation\n(provider, tokens, cost, schema)"),
    ("memory.memories", "corrections + lessons\nfeed back via RAG next iteration"),
    ("timeline_commits", "maps sacred_timeline ids → git SHA\n(provenance to source code)"),
];

fn safe_count(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |row| row.get::<_, i64>(0)).unwrap_or(0)
}

fn extend_els(out: &mut Vec<Value>, v: Value) {
    if let Value::Array(els) = v { out.extend(els); }
}

/// `export default function excalidrawLifecycle(input, context)`
pub fn excalidrawLifecycle(_input: &Value, db: &Connection) -> Result<Value, String> {
    let stats_cards = safe_count(db, "SELECT COUNT(*) AS n FROM cards");
    let stats_contracts = safe_count(db, "SELECT COUNT(*) AS n FROM contracts");
    let stats_phase_ev = safe_count(db, "SELECT COUNT(*) AS n FROM card_phase_events");
    let stats_laws = safe_count(db, "SELECT COUNT(*) AS n FROM laws.laws");

    let mut e: Vec<Value> = Vec::new();

    extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
        "x": 40, "y": 30,
        "text": "antiloki · canonical card lifecycle (PL → TL → DEV → QA → close)",
        "fontSize": 24, "color": C_ARROW,
    })));
    extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
        "x": 40, "y": 64,
        "text": format!("{stats_cards} cards lived through this · {stats_contracts} contracts written · {stats_phase_ev} phase events recorded · {stats_laws} laws gating each transition"),
        "fontSize": 12, "color": "#6e7681",
    })));
    extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
        "x": 40, "y": 84,
        "text": "each lane = one agent. each box = one canonical action. each arrow = a real call. each ledger label = a real INSERT.",
        "fontSize": 11, "color": "#9e9e9e",
    })));

    extend_els(&mut e, crate::render_excalidraw_emit::narrativeBlock(&json!({
        "x": 40, "y": 100, "width": 1900,
        "text":
            "WHAT THIS DIAGRAM SHOWS — the full life of a single card in antiloki, from raw intent to merged commit.\n\
             \n\
             A card is the unit of work. Its journey is five phases, each owned by a different agent. PL (atlas) turns a vague human intent into a draft: clarifies, researches, decomposes if needed, then approves a card row. TL (forge:arch) reviews that draft and writes the engineering contract — what files may be touched, what acceptance is, and (for medium+ work) a microtask plan that DEV will execute mechanically. DEV (forge:dev) loads the contract, runs cheap-tier LLM calls per microtask inside a copy-on-write sandbox, never escapes scope, and reports back. QA (warden) verifies the result with deterministic scanners — no AI is allowed to certify the work, only checking math is. git-manager seals it: requires all four phase events to be present (merge.gate), validates staged files are a subset of declared scope, then commits and marks the card done.\n\
             \n\
             HOW INFO TRANSFORMS toward the user goal: vague text → clarified questions → structured contract → microtask plan → patches in sandbox → verified output → audited git commit. Each handoff is gated by a law that can refuse the transition (gating-law footer in each lane). Each transition writes a phase_event the next agent can verify. By the time a card reaches done, seven independent ledgers agree the work was real. Run man.audit.truth.cross-check on any card to see those ledgers cross-checked in one call.",
    })));

    let driver_y: i64 = 360;
    let driver_h: i64 = 60;
    let total_w: i64 = 1900;
    extend_els(&mut e, crate::render_excalidraw_emit::rect(&json!({
        "x": 30, "y": driver_y, "width": total_w, "height": driver_h,
        "label": "DRIVER LOOP — wraps everything\ntick → evaluate candidates → law-check → fire action → log outcome → next tick · drives every transition below when ENFORCE=block",
        "fill": C_DRIVER, "fontSize": 12,
    })));

    let lane_y: i64 = driver_y + driver_h + 40;
    let lane_h: i64 = 720;
    let lane_w: i64 = 360;
    let lane_gap: i64 = 20;

    let lanes = lanes();
    let mut lane_x: i64 = 40;
    struct LaneCenter { left_x: i64, right_x: i64, mid_y: i64 }
    let mut lane_centers: Vec<LaneCenter> = Vec::new();

    for lane in &lanes {
        extend_els(&mut e, crate::render_excalidraw_emit::rect(&json!({
            "x": lane_x, "y": lane_y, "width": lane_w, "height": lane_h,
            "label": "", "fill": lane.color,
        })));
        extend_els(&mut e, crate::render_excalidraw_emit::rect(&json!({
            "x": lane_x, "y": lane_y, "width": lane_w, "height": 50,
            "label": format!("{}\n→ phase: {}", lane.agent, lane.title),
            "fill": "#ffffff", "fontSize": 13,
        })));

        let mut tool_y: i64 = lane_y + 60;
        for (name, desc) in lane.tools {
            extend_els(&mut e, crate::render_excalidraw_emit::rect(&json!({
                "x": lane_x + 14, "y": tool_y,
                "width": lane_w - 28, "height": 56,
                "label": format!("{name}\n{desc}"),
                "fill": C_ACTION, "fontSize": 9,
            })));
            tool_y += 64;
        }

        let reads_y = tool_y + 10;
        extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
            "x": lane_x + 14, "y": reads_y,
            "text": "◤ READS", "fontSize": 10, "color": "#6e7681",
        })));
        extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
            "x": lane_x + 14, "y": reads_y + 16,
            "text": lane.reads, "fontSize": 9, "color": "#1e1e1e",
        })));

        let writes_y = reads_y + 80;
        extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
            "x": lane_x + 14, "y": writes_y,
            "text": "◢ WRITES (audited)", "fontSize": 10, "color": C_ARROW_AUDIT,
        })));
        extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
            "x": lane_x + 14, "y": writes_y + 16,
            "text": lane.writes, "fontSize": 9, "color": "#1e1e1e",
        })));

        let gate_y = lane_y + lane_h - 50;
        extend_els(&mut e, crate::render_excalidraw_emit::rect(&json!({
            "x": lane_x + 14, "y": gate_y, "width": lane_w - 28, "height": 40,
            "label": format!("gated by: {}", lane.gate_law),
            "fill": C_GATE, "fontSize": 9,
        })));

        lane_centers.push(LaneCenter { left_x: lane_x, right_x: lane_x + lane_w, mid_y: lane_y + 25 });
        lane_x += lane_w + lane_gap;
    }

    for i in 0..lane_centers.len().saturating_sub(1) {
        let from = &lane_centers[i];
        let to = &lane_centers[i + 1];
        extend_els(&mut e, crate::render_excalidraw_emit::arrow(&json!({
            "x1": from.right_x, "y1": from.mid_y + 10,
            "x2": to.left_x, "y2": to.mid_y + 10,
            "label": format!("→ {}", lanes[i + 1].title),
            "stroke": C_ARROW,
        })));
    }

    let ribbon_y = lane_y + lane_h + 40;
    extend_els(&mut e, crate::render_excalidraw_emit::text(&json!({
        "x": 40, "y": ribbon_y,
        "text": "Ledgers (where the audit lives — read these to verify any claim about a card):",
        "fontSize": 14, "color": "#1e1e1e",
    })));
    let mut lx: i64 = 40;
    for (name, desc) in LEDGERS {
        extend_els(&mut e, crate::render_excalidraw_emit::rect(&json!({
            "x": lx, "y": ribbon_y + 26, "width": 250, "height": 80,
            "label": format!("{name}\n{desc}"), "fill": C_LEDGER, "fontSize": 10,
        })));
        lx += 270;
    }

    extend_els(&mut e, crate::render_excalidraw_emit::rect(&json!({
        "x": 40, "y": ribbon_y + 120, "width": 1900 - 60, "height": 50,
        "label": "→ man.audit.truth.cross-check (card_slug) — pulls all 7 sources for ONE card and asserts they agree. Returns critical_count + warning_count. If you don't trust the claims, run this. The data is the answer.",
        "fill": "#f1f8e9", "fontSize": 11,
    })));

    Ok(crate::render_excalidraw_emit::buildExcalidraw(
        &json!(e),
        &json!({ "name": "antiloki-card-lifecycle-2026-01-01" }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(db: &Connection) {
        db.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT);\
             CREATE TABLE contracts (id INTEGER PRIMARY KEY);\
             CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY);\
             ATTACH DATABASE ':memory:' AS laws;\
             CREATE TABLE laws.laws (id INTEGER PRIMARY KEY);"
        ).unwrap();
    }

    #[test]
    fn builds_full_diagram_with_header_lanes_ledgers_and_frozen_name() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        db.execute("INSERT INTO cards (slug) VALUES ('A-1'),('A-2')", []).unwrap();
        db.execute("INSERT INTO laws.laws DEFAULT VALUES", []).unwrap();

        let out = excalidrawLifecycle(&json!({}), &db).unwrap();
        assert_eq!(out["appState"]["name"], json!("antiloki-card-lifecycle-2026-01-01"));
        let els = out["elements"].as_array().unwrap();

        assert!(els.iter().any(|el| el.get("text").and_then(Value::as_str)
            == Some("2 cards lived through this · 0 contracts written · 0 phase events recorded · 1 laws gating each transition")));
        assert!(els.iter().any(|el| el.get("text").and_then(Value::as_str)
            .map_or(false, |t| t.starts_with("WHAT THIS DIAGRAM SHOWS"))));
        // 5 lanes each get a header text "AGENT\n→ phase: TITLE"
        assert!(els.iter().any(|el| el.get("text").and_then(Value::as_str) == Some("PL (atlas)\n→ phase: pl_draft")));
        assert!(els.iter().any(|el| el.get("text").and_then(Value::as_str) == Some("git-manager\n→ phase: close")));
        // 4 phase-transition arrows
        let arrow_labels: Vec<&str> = els.iter().filter_map(|el| {
            if el.get("type").and_then(Value::as_str) == Some("text") {
                el.get("text").and_then(Value::as_str).filter(|t| t.starts_with("→ "))
            } else { None }
        }).collect();
        assert!(arrow_labels.contains(&"→ tl_sign"));
        assert!(arrow_labels.contains(&"→ close"));
        // 7 ledger boxes
        for (name, _) in LEDGERS {
            assert!(els.iter().any(|el| el.get("text").and_then(Value::as_str).map_or(false, |t| t.starts_with(&format!("{name}\n")))));
        }
    }

    #[test]
    fn missing_laws_table_counts_as_zero() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY);\
             CREATE TABLE contracts (id INTEGER PRIMARY KEY);\
             CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY);"
        ).unwrap();
        let out = excalidrawLifecycle(&json!({}), &db).unwrap();
        let els = out["elements"].as_array().unwrap();
        assert!(els.iter().any(|el| el.get("text").and_then(Value::as_str)
            == Some("0 cards lived through this · 0 contracts written · 0 phase events recorded · 0 laws gating each transition")));
    }
}
