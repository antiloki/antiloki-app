//! contract-gate-tl-sign.rs — twin of skills/agents/tl/contract-gate-tl-sign.mjs (db WRITE lane,
//! attached `memory` db via the reject branch's learnInsight call). NAMES ARE CONTRACT:
//! `contractGateTlSign` (default). Two outcomes: decision='accept' persists/updates the contract
//! row to status='tl_approved' (via contractAdd or a direct UPDATE when the row already exists at
//! pl_ratified/proposed) and best-effort fires a tl_sign card_phase_event; decision='reject' records
//! a correction memory via learnInsight (the self-learning loop). Guard-throws (missing briefing,
//! sanity-gate violations, missing microtasks, already-signed, etc.) are NOT exercised by the
//! fixture — only the success path is judged (playbook gotcha #9).
//!
//! Cross-module (all CONVERTED twins): contractAdd (contract/contract-add.mjs), contractSanityCheck
//! (contract/contract-sanity-check.mjs), cardPhaseEventRecord (entities/card-phase-event-record.mjs),
//! devPlanSpecValidate (agents/dev/dev-plan-spec.mjs), authoredBlockCount (swarm/swarm-block-presence.mjs),
//! learnInsight (ai/learn-insight.mjs).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
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
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn safe_parse_array(s: &str) -> Value {
    match serde_json::from_str::<Value>(s) {
        Ok(v) if v.is_array() => v,
        _ => json!([]),
    }
}

// microtasks = Array.isArray(contract.microtasks) ? contract.microtasks
//   : (typeof contract.microtasks_json === 'string' ? safeParseArray(contract.microtasks_json)
//      : (Array.isArray(contract.microtasks_json) ? contract.microtasks_json : []))
fn compute_microtasks(contract: &Value) -> Value {
    if let Some(a) = contract.get("microtasks").filter(|v| v.is_array()) {
        return a.clone();
    }
    match contract.get("microtasks_json") {
        Some(Value::String(s)) => safe_parse_array(s),
        Some(v) if v.is_array() => v.clone(),
        _ => json!([]),
    }
}

// Best-effort: swallow all errors, mirroring the JS try/catch.
fn mark_contract_rejected(db: &Connection, slug: &Value, failure_kind: &str, reason_md: &str) {
    let slug_s = match slug.as_str() {
        Some(s) if !s.is_empty() => s,
        _ => return,
    };
    let _ = crate::db_json::run_write(
        db,
        &format!("UPDATE contracts SET status = 'rejected', rejection_reason_md = ?, failure_kind = ?, updated_at = {} WHERE slug = ? AND status NOT IN ('rejected', 'superseded')", crate::clock::now_sql()),
        &[json!(reason_md), json!(failure_kind), json!(slug_s)],
    );
}

pub fn contractGateTlSign(input: &Value, db: &Connection) -> Result<Value, String> {
    let empty = json!({});
    let input = if input.is_object() { input } else { &empty };

    let contract = input.get("contract").cloned().unwrap_or(Value::Null);
    let decision = input.get("decision").cloned().unwrap_or(Value::Null);
    let missing_fields = match input.get("missing_fields") { None => json!([]), Some(v) => v.clone() };
    let reason = input.get("reason").cloned().unwrap_or(Value::Null);
    let signed_by = match input.get("signed_by") { None => json!("agent:tl"), Some(v) => v.clone() };
    let tl_briefing_md = input.get("tl_briefing_md").cloned().unwrap_or(Value::Null);
    let tl_evaluation_md = input.get("tl_evaluation_md").cloned().unwrap_or(Value::Null);

    if !contract.is_object() {
        return Err("contract-gate-tl-sign: 'contract' is required".into());
    }
    let slug = contract.get("slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&slug)) {
        return Err("contract-gate-tl-sign: contract.slug missing — contract is malformed".into());
    }
    if !truthy(Some(&decision)) {
        return Err("contract-gate-tl-sign: 'decision' is required".into());
    }
    let decision_s = decision.as_str().unwrap_or("");
    if decision_s != "accept" && decision_s != "reject" {
        return Err(format!("contract-gate-tl-sign: 'decision' must be one of: accept, reject (got '{}')", tmpl(Some(&decision))));
    }

    if decision_s == "accept" {
        let tier = contract.get("tier").cloned().unwrap_or(Value::Null);
        let size = contract.get("size").cloned().unwrap_or(Value::Null);
        let requires_briefing = truthy(Some(&tier)) && tier.as_str() != Some("trivial") && size.as_str() != Some("XS");
        let briefing_len = tl_briefing_md.as_str().map(|s| s.trim().chars().count()).unwrap_or(0);
        if requires_briefing && (!truthy(Some(&tl_briefing_md)) || briefing_len < 20) {
            let cause = format!(
                "tl_briefing_md required (>=20 chars) for tier='{}', size='{}'. TL must connect card intent -> contract spec in prose.",
                tmpl(Some(&tier)), tmpl(Some(&size))
            );
            mark_contract_rejected(db, &slug, "tl_briefing_missing", &format!("# Refused at contract.gate.tl.sign\n\n**Reason:** {}\n", cause));
            return Err(cause);
        }

        let sanity = crate::contract_contract_sanity_check::contractSanityCheck(&contract, db)?;
        if !truthy(sanity.get("ok")) {
            let violations = sanity.get("violations").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            mark_contract_rejected(
                db,
                &slug,
                "sanity_gate",
                &format!("# Refused at contract.gate.tl.sign · sanity gate\n\n**{} violation(s):**\n", violations.len()),
            );
            return Err(format!("sanity gate refused tl_approved: {} violation(s)", violations.len()));
        }

        let size_s = size.as_str().unwrap_or("");
        let requires_microtasks = size_s == "M" || size_s == "L";
        let block_count = crate::swarm_swarm_block_presence::authoredBlockCount(db, slug.as_str().unwrap_or(""))
            .as_i64()
            .unwrap_or(0);
        if requires_microtasks && block_count == 0 {
            let microtasks = compute_microtasks(&contract);
            let mts = microtasks.as_array().cloned().unwrap_or_default();
            if mts.is_empty() {
                let cause = format!("microtasks_json required for size='{}'. Run tl.refine.microtasks before signing.", size_s);
                mark_contract_rejected(db, &slug, "microtasks_missing", &format!("# Refused at contract.gate.tl.sign · microtasks_missing\n\n**Reason:** {}\n", cause));
                return Err(cause);
            }
            let total: f64 = mts.iter().map(|m| m.get("max_cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0)).sum::<f64>() + 1e-6;
            let card_slug_for_check = nz(contract.get("card_slug")).cloned().unwrap_or_else(|| slug.clone());
            let plan_for_check = json!({
                "plan": {
                    "plan_id": format!("sign-check-{}", slug.as_str().unwrap_or("")),
                    "card_slug": card_slug_for_check,
                    "total_max_cost_usd": total,
                    "microtasks": mts,
                }
            });
            if let Err(validation_error) = crate::agents_dev_dev_plan_spec::devPlanSpecValidate(&plan_for_check, db) {
                let cause = format!("microtask decomposition invalid: {}", validation_error);
                mark_contract_rejected(
                    db,
                    &slug,
                    "microtask_decomp_invalid",
                    &format!("# Refused at contract.gate.tl.sign · microtask_decomp_invalid\n\n**Validator output:**\n\n```\n{}\n```\n", validation_error),
                );
                return Err(cause);
            }
        }

        let signed_at = json!(crate::clock::now_iso_nodb());
        let microtasks_for_persist = compute_microtasks(&contract);
        let persist_input = json!({
            "slug": slug.clone(),
            "title": contract.get("title").cloned().unwrap_or(Value::Null),
            "status": "tl_approved",
            "template_kind": nz(contract.get("template_kind")).cloned().unwrap_or(Value::Null),
            "size": nz(contract.get("size")).cloned().unwrap_or(Value::Null),
            "complexity": nz(contract.get("complexity")).cloned().unwrap_or(Value::Null),
            "risk": nz(contract.get("risk")).cloned().unwrap_or(Value::Null),
            "tier": nz(contract.get("tier")).cloned().unwrap_or(Value::Null),
            "card_slug": nz(contract.get("card_slug")).cloned().unwrap_or(Value::Null),
            "files_touched": nz(contract.get("files_touched")).cloned().unwrap_or(json!([])),
            "acceptance_json": nz(contract.get("acceptance_json")).cloned().unwrap_or(json!([])),
            "naming": nz(contract.get("naming")).cloned().unwrap_or(json!({})),
            "edge_cases": nz(contract.get("edge_cases")).cloned().unwrap_or(json!([])),
            "must_obey": nz(contract.get("must_obey")).cloned().unwrap_or(json!([])),
            "out_of_scope": nz(contract.get("out_of_scope")).cloned().unwrap_or(json!([])),
            "reference_artifacts": nz(contract.get("reference_artifacts")).cloned().unwrap_or(json!([])),
            "briefing_md": nz(contract.get("briefing_md")).cloned().unwrap_or(Value::Null),
            "base_sha": nz(contract.get("base_sha")).cloned().unwrap_or(Value::Null),
            "current_attempt": nz(contract.get("current_attempt")).cloned().unwrap_or(json!(1)),
            "signed_by": signed_by.clone(),
            "signed_at": signed_at.clone(),
            "tl_briefing_md": nz(Some(&tl_briefing_md)).cloned().unwrap_or(Value::Null),
            "tl_evaluation_md": nz(Some(&tl_evaluation_md)).cloned().unwrap_or(Value::Null),
            "microtasks": microtasks_for_persist,
        });

        let existing_rows = crate::db_json::query_rows(db, "SELECT id, status FROM contracts WHERE slug = ?", &[slug.clone()])?;
        let existing = existing_rows.as_array().and_then(|a| a.first()).cloned();

        let persisted: Value;
        if let Some(row) = existing {
            let status = row.get("status").cloned().unwrap_or(Value::Null);
            let status_s = status.as_str().unwrap_or("");
            if status_s != "pl_ratified" && status_s != "proposed" {
                return Err(format!("contract '{}' already exists — to revise, supersede it", slug.as_str().unwrap_or("")));
            }
            crate::db_json::run_write(
                db,
                &format!("UPDATE contracts SET status = 'tl_approved', signed_by = ?, signed_at = ?, tl_briefing_md = ?, tl_evaluation_md = ?, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
                &[signed_by.clone(), signed_at.clone(), nz(Some(&tl_briefing_md)).cloned().unwrap_or(Value::Null), nz(Some(&tl_evaluation_md)).cloned().unwrap_or(Value::Null), slug.clone()],
            )?;
            persisted = json!({ "id": row.get("id").cloned().unwrap_or(Value::Null), "slug": slug.clone(), "status": "tl_approved" });
        } else {
            match crate::contract_contract_add::contractAdd(&persist_input, db) {
                Ok(row) => persisted = row,
                Err(e) => {
                    if e.to_uppercase().contains("UNIQUE") || e.to_lowercase().contains("rejected") {
                        return Err(format!("contract '{}' already exists — to revise, supersede it", slug.as_str().unwrap_or("")));
                    }
                    return Err(e);
                }
            }
        }

        let card_slug = contract.get("card_slug").cloned().unwrap_or(Value::Null);
        let mut phase_event_id = Value::Null;
        if truthy(Some(&card_slug)) {
            let ev = crate::entities_card_phase_event_record::cardPhaseEventRecord(
                &json!({
                    "card_slug": card_slug,
                    "phase": "tl_sign",
                    "actor": signed_by.clone(),
                    "evidence": { "source": "contract-gate-tl-sign", "contract_slug": slug, "signed_at": signed_at.clone() },
                }),
                db,
            );
            if let Ok(ev) = ev {
                phase_event_id = ev.get("id").cloned().unwrap_or(Value::Null);
            }
        }

        let mut contract_out = contract.as_object().cloned().unwrap_or_default();
        contract_out.insert("signed_by".into(), signed_by.clone());
        contract_out.insert("signed_at".into(), signed_at.clone());
        contract_out.insert("status".into(), json!("tl_approved"));

        let mut out = Map::new();
        out.insert("accepted".into(), json!(true));
        out.insert("persisted".into(), json!(true));
        out.insert("contract".into(), Value::Object(contract_out));
        out.insert("persisted_row_id".into(), nz(persisted.get("id")).cloned().unwrap_or(Value::Null));
        out.insert("phase_event_id".into(), phase_event_id);
        return Ok(Value::Object(out));
    }

    // ── reject branch: feed the correction memory ────────────────────────
    let missing_arr = missing_fields.as_array().cloned().unwrap_or_default();
    if missing_arr.is_empty() {
        return Err("contract-gate-tl-sign: reject requires missing_fields[] (at least one)".into());
    }

    // Two DIFFERENT ?? fallbacks in the JS source for the same field: guidance text uses
    // 'none', the tags array element uses 'no-template'.
    let template_kind_opt = contract.get("template_kind");
    let template_kind_for_guidance = match template_kind_opt { Some(v) if !v.is_null() => v.clone(), _ => json!("none") };
    let template_kind_for_tags = match template_kind_opt { Some(v) if !v.is_null() => v.clone(), _ => json!("no-template") };
    let missing_joined = missing_arr.iter().map(|v| tmpl(Some(v))).collect::<Vec<_>>().join(", ");
    let mut guidance = format!(
        "Contract '{}' (template_kind='{}') was rejected by TL. Missing/wrong: {}.",
        tmpl(Some(&slug)), tmpl(Some(&template_kind_for_guidance)), missing_joined
    );
    if truthy(Some(&reason)) {
        guidance.push_str(&format!(" Reason: {}", tmpl(Some(&reason))));
    }
    // `${contract.card_slug}` — a plain template literal, NOT `??`: an absent key renders "undefined".
    let situation = format!(
        "auto-drafted contract for card '{}' with template_kind='{}'",
        tmpl(contract.get("card_slug")), tmpl(Some(&template_kind_for_guidance))
    );
    // `contract.files_touched`/`contract.confidence` are plain property reads in JS: an ABSENT
    // key (contract.files_touched === undefined) is OMITTED by JSON.stringify, but an explicit
    // `null` value is kept. Only insert the key when the source actually has it (gotcha #3).
    let mut naive_response_map = Map::new();
    naive_response_map.insert("slug".into(), slug.clone());
    if let Some(v) = contract.get("files_touched") {
        naive_response_map.insert("files_touched".into(), v.clone());
    }
    naive_response_map.insert(
        "acceptance_count".into(),
        json!(contract.get("acceptance_json").and_then(|v| v.as_array()).map(|a| a.len() as i64).unwrap_or(0)),
    );
    naive_response_map.insert(
        "edge_cases_count".into(),
        json!(contract.get("edge_cases").and_then(|v| v.as_array()).map(|a| a.len() as i64).unwrap_or(0)),
    );
    if let Some(v) = contract.get("confidence") {
        naive_response_map.insert("confidence".into(), v.clone());
    }
    let naive_response_obj = Value::Object(naive_response_map);
    let naive_response: String = serde_json::to_string(&naive_response_obj).unwrap_or_default().chars().take(800).collect();

    let insight = crate::ai_learn_insight::learnInsight(
        &json!({
            "moment_kind": "rejection",
            "emitter": "contract.gate.tl.sign",
            "source_ref": slug.clone(),
            "situation_md": situation,
            "naive_md": naive_response,
            "corrected_md": guidance,
            "tags": ["contract", "tl-rejection", template_kind_for_tags],
            "created_by": signed_by,
        }),
        db,
    )
    .map_err(|e| format!("failed to record learning insight: {e}"))?;

    let mut out = Map::new();
    out.insert("accepted".into(), json!(false));
    out.insert("contract_slug".into(), slug);
    out.insert("missing_fields".into(), missing_fields);
    out.insert("reason".into(), reason);
    out.insert("memory_id".into(), nz(insight.get("memory_id")).cloned().unwrap_or(Value::Null));
    out.insert("insight_path".into(), insight.get("insight_path").cloned().unwrap_or(Value::Null));
    out.insert("links_added".into(), insight.get("links_added").cloned().unwrap_or(Value::Null));
    out.insert("consolidation_triggered".into(), insight.get("consolidation_triggered").cloned().unwrap_or(Value::Null));
    out.insert("self_learning".into(), json!("recorded via system.learn.insight; RAG-auto-linked to related corrections"));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
            CREATE TABLE contracts (
              id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, title TEXT NOT NULL,
              status TEXT NOT NULL DEFAULT 'proposed', template_kind TEXT, size TEXT, complexity INTEGER,
              risk INTEGER, tier TEXT, card_slug TEXT, files_touched_json TEXT NOT NULL DEFAULT '[]',
              acceptance_json TEXT NOT NULL DEFAULT '[]', naming_json TEXT NOT NULL DEFAULT '{}',
              edge_cases_json TEXT NOT NULL DEFAULT '[]', must_obey_json TEXT NOT NULL DEFAULT '[]',
              out_of_scope_json TEXT NOT NULL DEFAULT '[]', reference_artifacts_json TEXT NOT NULL DEFAULT '[]',
              briefing_md TEXT, base_sha TEXT, current_attempt INTEGER NOT NULL DEFAULT 1, superseded_by TEXT,
              signed_by TEXT, signed_at TEXT, tl_briefing_md TEXT, tl_evaluation_md TEXT,
              microtasks_json TEXT NOT NULL DEFAULT '[]', goal_md TEXT, rejection_reason_md TEXT, failure_kind TEXT,
              updated_at TEXT);
            CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT, updated_at TEXT);
            CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY, card_slug TEXT, phase TEXT, actor TEXT, evidence_json TEXT);
            CREATE TABLE swarm_manifests (id INTEGER, contract_slug TEXT, superseded_by TEXT, version INTEGER);
            CREATE TABLE swarm_blocks (manifest_id INTEGER);
            CREATE TABLE memory.catalog_embeddings (entity_type TEXT NOT NULL, entity_name TEXT NOT NULL, model TEXT NOT NULL, dims INTEGER NOT NULL, vector BLOB NOT NULL);
            CREATE TABLE memory.memories (
              id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL, situation TEXT NOT NULL,
              guidance TEXT NOT NULL, naive_response TEXT, rationale TEXT, tags_json TEXT NOT NULL DEFAULT '[]',
              source_card TEXT, source_event INTEGER, source_session TEXT, confidence INTEGER NOT NULL DEFAULT 0,
              validated INTEGER NOT NULL DEFAULT 0, superseded_by INTEGER, agent_slug TEXT,
              created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), created_by TEXT NOT NULL,
              CHECK (type IN ('lesson', 'preference', 'decision', 'reference', 'correction', 'narrative')),
              CHECK (confidence >= 0 AND confidence <= 1), CHECK (validated IN (0, 1)));
            CREATE TABLE memory.memory_links (
              id INTEGER PRIMARY KEY AUTOINCREMENT, from_memory_id INTEGER NOT NULL, to_memory_id INTEGER NOT NULL,
              kind TEXT NOT NULL, rationale TEXT, weight REAL NOT NULL DEFAULT 1.0,
              created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), created_by TEXT NOT NULL,
              CHECK (kind IN ('contradicts', 'extends', 'derived_from', 'related', 'corrects')),
              CHECK (from_memory_id != to_memory_id), CHECK (weight >= 0.0 AND weight <= 1.0),
              UNIQUE (from_memory_id, to_memory_id, kind));",
        )
        .unwrap();
        c
    }

    #[test]
    fn accept_trivial_tier_inserts_new_contract() {
        let c = db();
        let r = contractGateTlSign(
            &json!({ "contract": { "slug": "CT-1", "title": "T1", "tier": "trivial", "size": "S", "files_touched": ["a.ts"] }, "decision": "accept" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["accepted"], json!(true));
        assert_eq!(r["contract"]["status"], json!("tl_approved"));
        assert_eq!(r["contract"]["signed_by"], json!("agent:tl"));
        assert!(r["phase_event_id"].is_null());
        let st: String = c.query_row("SELECT status FROM contracts WHERE slug='CT-1'", [], |row| row.get(0)).unwrap();
        assert_eq!(st, "tl_approved");
    }

    #[test]
    fn accept_updates_existing_pl_ratified_row_and_fires_phase_event() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, title, status, card_slug) VALUES ('CT-2','T2','pl_ratified','CARD-1')", []).unwrap();
        c.execute("INSERT INTO cards (slug, status) VALUES ('CARD-1','doing')", []).unwrap();
        let r = contractGateTlSign(
            &json!({
                "contract": { "slug": "CT-2", "title": "T2", "tier": "standard", "size": "S", "card_slug": "CARD-1", "files_touched": ["a.ts"] },
                "decision": "accept",
                "tl_briefing_md": "connects card intent to contract spec clearly"
            }),
            &c,
        )
        .unwrap();
        assert_eq!(r["accepted"], json!(true));
        assert_eq!(r["persisted_row_id"], json!(1));
        assert_eq!(r["phase_event_id"], json!(1));
        let ev: String = c.query_row("SELECT phase FROM card_phase_events WHERE id=1", [], |row| row.get(0)).unwrap();
        assert_eq!(ev, "tl_sign");
    }

    #[test]
    fn accept_size_m_with_authored_blocks_skips_microtask_requirement() {
        let c = db();
        c.execute("INSERT INTO swarm_manifests VALUES (5,'CT-3',NULL,1)", []).unwrap();
        c.execute("INSERT INTO swarm_blocks VALUES (5)", []).unwrap();
        let r = contractGateTlSign(
            &json!({ "contract": { "slug": "CT-3", "title": "T3", "tier": "trivial", "size": "M", "files_touched": ["a.ts"] }, "decision": "accept" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["accepted"], json!(true));
    }

    #[test]
    fn accept_size_m_with_valid_microtasks_persists() {
        let c = db();
        let mt = json!({ "microtask_id": "m1", "intent": "do it", "files_touched": ["a.ts"], "acceptance": ["x"], "max_tokens": 512, "max_cost_usd": 0.05, "expected_diff_shape": "small_edit" });
        let r = contractGateTlSign(
            &json!({
                "contract": { "slug": "CT-4", "title": "T4", "tier": "trivial", "size": "M", "files_touched": ["a.ts"], "microtasks": [mt] },
                "decision": "accept"
            }),
            &c,
        )
        .unwrap();
        assert_eq!(r["accepted"], json!(true));
        let mj: String = c.query_row("SELECT microtasks_json FROM contracts WHERE slug='CT-4'", [], |row| row.get(0)).unwrap();
        assert!(mj.contains("m1"));
    }

    #[test]
    fn reject_records_correction_memory() {
        let c = db();
        let r = contractGateTlSign(
            &json!({
                "contract": { "slug": "CT-5", "card_slug": "CARD-9", "template_kind": "feature" },
                "decision": "reject",
                "missing_fields": ["acceptance_json"],
                "reason": "too vague"
            }),
            &c,
        )
        .unwrap();
        assert_eq!(r["accepted"], json!(false));
        assert_eq!(r["contract_slug"], json!("CT-5"));
        assert_eq!(r["memory_id"], json!(1));
        assert_eq!(r["self_learning"], json!("recorded via system.learn.insight; RAG-auto-linked to related corrections"));
        let ty: String = c.query_row("SELECT type FROM memory.memories WHERE id=1", [], |row| row.get(0)).unwrap();
        assert_eq!(ty, "correction");
    }
}
