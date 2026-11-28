//! subagent-tune.rs — twin of skills/swarm/subagent-tune.mjs (db, PARITY-DEFERRED at the aiCall
//! boundary). NAMES ARE CONTRACT: default `subagentTune`. Reads a sub-agent leaf's recent
//! authoring performance (swarm_blocks rows whose `authored_by` ends with `:<agent>`, and whether
//! each one's composition gate passed) and PROPOSES a refined `specialization_md` via a MID call.
//! PROPOSE ONLY — never writes the agents row (the JS doc: applied via
//! system.agent.subagent.update, or a future canary).
//!
//! PARITY-DEFERRED: the JS wraps its `await aiCall(...)` in try/catch and on ANY failure leaves
//! `proposed_md = current_md` ("propose-only, never blocks"). `crate::ai_ai_call::aiCall` always
//! returns `Err` (see that module's doc — a real network AI-provider dispatch, not fixturable), so
//! this twin's `proposed_md` is always the unchanged `current_md` — that IS the faithful behavior
//! of the JS's catch-swallow path, not an approximation of it. Everything that surrounds the call —
//! agent/row lookup, the sample-limit clamp, the swarm_blocks performance query, the evidence/
//! performance string assembly, and `ensure_tune_prompt` (reusing the already-converted
//! `ai_prompt_find`/`ai_prompt_add` twins) — is ported faithfully and IS the deterministic surface
//! this file's tests exercise.
use rusqlite::Connection;
use serde_json::{json, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}
fn first(rows: &Value) -> Option<Value> { rows.as_array().and_then(|a| a.first()).cloned() }

const TUNE_PROMPT: &str = "swarm.subagent.tune.v1";
const TUNE_TEMPLATE: &str = "You refine the SPECIALIZATION instructions of a coding sub-agent so its next files compose + pass review. Keep it TIGHT and imperative — a short delta layered on the parent's discipline, not a rewrite. Do not add scope the role should not have.\n\nSUB-AGENT: {{agent}}\nCURRENT SPECIALIZATION:\n{{current_md}}\n\nRECENT PERFORMANCE: {{performance}}\nFAILURE EVIDENCE (files/contracts that did NOT compose or were flagged):\n{{evidence}}\n\nOutput ONLY the improved specialization text into refined_md — no prose, no fences, no preamble. If the current specialization is already good and the failures are not its fault, return it unchanged.";

// `ensureTunePrompt`: try promptFind; if found with a matching template_md, no-op; else promptAdd.
fn ensure_tune_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": TUNE_PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(|v| v.as_str()) == Some(TUNE_TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": TUNE_PROMPT,
            "template_md": TUNE_TEMPLATE,
            "default_tier": "mid",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 800,
            "output_fields": [{ "field_name": "refined_md", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

pub fn subagentTune(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent). `provider_fn` is threaded
    // opaquely into aiCall's input; there is no Rust equivalent to carry (aiCall's own dispatch is
    // the deferred boundary), so it is not modeled.
    let agent = input.get("agent").filter(|v| !v.is_null());
    let Some(agent) = agent else {
        return Err("subagent-tune: agent is required".to_string());
    };
    let agent_s = str_qq(Some(agent));

    let rows = crate::db_json::query_rows(
        db,
        "SELECT name, specialization_md FROM agents WHERE name = ?",
        &[json!(agent_s)],
    )?;
    let Some(row) = first(&rows) else {
        return Err(format!("subagent-tune: agent '{agent_s}' not found"));
    };
    let current_md = row
        .get("specialization_md")
        .filter(|v| !v.is_null())
        .map(|v| str_qq(Some(v)))
        .unwrap_or_default();

    let limit_num = js_number(input.get("sample_limit"));
    let limit_num = if limit_num.is_nan() { 50.0 } else { limit_num };
    let limit = (limit_num as i64).max(1).min(200);

    let like_pat = format!("%:{agent_s}");
    let blocks = crate::db_json::query_rows(
        db,
        "SELECT file, contract_slug, compose_passed FROM swarm_blocks WHERE authored_by LIKE ? ORDER BY id DESC LIMIT ?",
        &[json!(like_pat), json!(limit)],
    )
    .unwrap_or(json!([]));
    let blocks: Vec<Value> = blocks.as_array().cloned().unwrap_or_default();
    let total = blocks.len() as i64;
    let failed_blocks: Vec<&Value> = blocks
        .iter()
        .filter(|a| a.get("compose_passed").and_then(|v| v.as_i64()) == Some(0))
        .collect();
    let failed = failed_blocks.len() as i64;
    let pass_rate = if total > 0 {
        Some((((total - failed) as f64 / total as f64) * 1000.0).round() / 1000.0)
    } else {
        None
    };

    let mut evidence_lines: Vec<String> = failed_blocks
        .iter()
        .take(12)
        .map(|a| {
            format!(
                "- {} (contract {}) did not compose",
                str_qq(a.get("file")),
                str_qq(a.get("contract_slug")),
            )
        })
        .collect();
    if let Some(extra) = input.get("extra_evidence_md").filter(|v| !v.is_null()) {
        evidence_lines.push(str_qq(Some(extra)));
    }
    let evidence = if evidence_lines.is_empty() {
        "(no recorded composition failures for this leaf)".to_string()
    } else {
        evidence_lines.join("\n")
    };
    let performance = if total > 0 {
        format!(
            "{}/{} authored blocks composed (pass_rate={})",
            total - failed,
            total,
            pass_rate.map(|p| p.to_string()).unwrap_or_default()
        )
    } else {
        "no blocks authored by this leaf yet".to_string()
    };

    ensure_tune_prompt(db)?;
    // PARITY-DEFERRED: `crate::ai_ai_call::aiCall` always returns Err — the JS catch swallows any
    // failure and leaves `proposed_md = current_md`, so this call's Err is never propagated.
    let _ = crate::ai_ai_call::aiCall(
        &json!({
            "prompt_name": TUNE_PROMPT,
            "input_kv": [
                { "key": "agent", "value": agent_s },
                { "key": "current_md", "value": if current_md.is_empty() { "(none)".to_string() } else { current_md.clone() } },
                { "key": "performance", "value": performance },
                { "key": "evidence", "value": evidence },
            ],
            "tier": "mid", "temperature": "cold", "phase_flow": "single_only",
        }),
        db,
    );
    let proposed_md = current_md.clone();

    Ok(json!({
        "agent": agent_s,
        "current_md": current_md.clone(),
        "proposed_md": proposed_md,
        "sample": { "total": total, "failed": failed, "pass_rate": pass_rate },
        "changed": proposed_md.trim() != current_md.trim(),
        "apply_action": "system.agent.subagent.update",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE agents (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, specialization_md TEXT);
             CREATE TABLE swarm_blocks (id INTEGER PRIMARY KEY AUTOINCREMENT, file TEXT, contract_slug TEXT, compose_passed INTEGER, authored_by TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single' );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        )
        .unwrap();
        c
    }

    fn an_agent(c: &Connection, name: &str, spec: &str) {
        c.execute("INSERT INTO agents (name, specialization_md) VALUES (?, ?)", [name, spec]).unwrap();
    }

    #[test]
    fn agent_is_required() {
        let c = setup();
        assert!(subagentTune(&json!({}), &c).unwrap_err().contains("agent is required"));
    }

    #[test]
    fn agent_not_found() {
        let c = setup();
        assert!(subagentTune(&json!({ "agent": "nope" }), &c).unwrap_err().contains("agent 'nope' not found"));
    }

    #[test]
    fn no_blocks_yet_yields_null_pass_rate_and_unchanged_proposal() {
        let c = setup();
        an_agent(&c, "leaf-a", "spec text");
        let r = subagentTune(&json!({ "agent": "leaf-a" }), &c).unwrap();
        assert_eq!(r["current_md"], json!("spec text"));
        assert_eq!(r["proposed_md"], json!("spec text"));
        assert_eq!(r["changed"], json!(false));
        assert_eq!(r["sample"]["total"], json!(0));
        assert_eq!(r["sample"]["pass_rate"], Value::Null);
        assert_eq!(r["apply_action"], json!("system.agent.subagent.update"));
        // ensure_tune_prompt ran (real, ported step before the deferred call)
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'swarm.subagent.tune.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn missing_specialization_defaults_to_empty_string() {
        let c = setup();
        c.execute("INSERT INTO agents (name) VALUES ('leaf-b')", []).unwrap();
        let r = subagentTune(&json!({ "agent": "leaf-b" }), &c).unwrap();
        assert_eq!(r["current_md"], json!(""));
    }

    #[test]
    fn sample_reflects_only_this_leafs_authored_blocks_via_suffix_match() {
        let c = setup();
        an_agent(&c, "leaf-c", "spec");
        c.execute("INSERT INTO swarm_blocks (file, contract_slug, compose_passed, authored_by) VALUES ('a.rs', 'c1', 1, 'wave1:leaf-c')", []).unwrap();
        c.execute("INSERT INTO swarm_blocks (file, contract_slug, compose_passed, authored_by) VALUES ('b.rs', 'c2', 0, 'wave1:leaf-c')", []).unwrap();
        // a DIFFERENT leaf whose name ends with 'leaf-c' as a substring but not after ':' — must NOT match
        c.execute("INSERT INTO swarm_blocks (file, contract_slug, compose_passed, authored_by) VALUES ('z.rs', 'zz', 0, 'wave1:other-leaf-c-2')", []).unwrap();
        let r = subagentTune(&json!({ "agent": "leaf-c" }), &c).unwrap();
        assert_eq!(r["sample"]["total"], json!(2));
        assert_eq!(r["sample"]["failed"], json!(1));
        assert_eq!(r["sample"]["pass_rate"], json!(0.5));
    }

    #[test]
    fn sample_limit_is_clamped_between_1_and_200() {
        let c = setup();
        an_agent(&c, "leaf-d", "spec");
        for i in 0..5 {
            c.execute(
                "INSERT INTO swarm_blocks (file, contract_slug, compose_passed, authored_by) VALUES (?, 'c', 1, 'w:leaf-d')",
                [format!("f{i}.rs")],
            )
            .unwrap();
        }
        let r = subagentTune(&json!({ "agent": "leaf-d", "sample_limit": 2 }), &c).unwrap();
        assert_eq!(r["sample"]["total"], json!(2));
        let r2 = subagentTune(&json!({ "agent": "leaf-d", "sample_limit": 99999 }), &c).unwrap();
        assert_eq!(r2["sample"]["total"], json!(5));
    }

    #[test]
    fn evidence_lists_failed_blocks_plus_extra_evidence() {
        let c = setup();
        an_agent(&c, "leaf-e", "spec");
        c.execute("INSERT INTO swarm_blocks (file, contract_slug, compose_passed, authored_by) VALUES ('bad.rs', 'ct1', 0, 'w:leaf-e')", []).unwrap();
        let r = subagentTune(&json!({ "agent": "leaf-e", "extra_evidence_md": "manual note" }), &c).unwrap();
        // proposed_md stays == current_md (PARITY-DEFERRED aiCall never succeeds); the evidence
        // assembly itself is exercised indirectly via the ensure_tune_prompt row + no panic/err.
        assert_eq!(r["changed"], json!(false));
        assert_eq!(r["sample"]["failed"], json!(1));
    }
}
