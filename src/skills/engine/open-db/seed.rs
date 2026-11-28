//! seed.rs — twin of skills/engine/open-db/seed.mjs: canonical seeding (agents, actions +
//! specs, agent_stages, ai_providers, routing_defaults, laws, scoring_rubric, workflows,
//! heimdall, env_flags, + the law-trigger application). NAMES ARE CONTRACT: seedCanonical,
//! seedEnvFlags, seedWorkflows, seedAgentStages, seedAiProviders, seedRoutingDefaults,
//! seedCanonicalLaws, seedScoringRubric, applyCanonicalLawTriggers.
//!
//! All 9 exports take a single `db` argument (db-first helper, zero extra positional args) —
//! same shape as engine_canonical_heimdall::seedHeimdall. CANONICAL_AGENTS / CANONICAL_ACTIONS
//! / CANONICAL_LAWS / CANONICAL_AGENT_STAGES / CANONICAL_ENV_FLAGS are already pressed statics
//! in their own twins (engine_canonical_agents / engine_canonical_actions / engine_canonical_laws
//! / engine_canonical_agent_stages / engine_canonical_env_flags) — reused verbatim here, so the
//! only NEW data this file carries is CANONICAL_ACTION_SPECS (never twinned elsewhere, since
//! canonical-actions.mjs's own conversion only covers the function-shaped CANONICAL_ACTIONS
//! export; the object-literal CANONICAL_ACTION_SPECS const is data, not a unit, but seedCanonical
//! iterates it directly so it must be pressed here too). seedHeimdall is called as the already-
//! converted cross-module twin (crate::engine_canonical_heimdall::seedHeimdall).
//!
//! datetime('now') → frozen '2026-01-01 00:00:00' per the frozen-clock lane.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::LazyLock;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn rows_nonempty(v: &Result<Value, String>) -> bool {
    v.as_ref().map(|r| r.as_array().map_or(false, |a| !a.is_empty())).unwrap_or(false)
}

// CANONICAL_ACTION_SPECS — twin of the JS object literal in canonical-actions.mjs (verification_json
// per action). Object key order is preserved (serde preserve_order) so JSON.stringify(spec) matches.
pub static CANONICAL_ACTION_SPECS: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(r#"{
  "board-keeper.card.add": {
    "postcondition": [{ "kind": "sql", "sql": "SELECT slug FROM cards WHERE slug = 'AV-CARD-ADD-TEST'", "expect_min_rows": 1 }],
    "audit_assertion": [{ "operation": "card.add", "entity_type": "card" }],
    "replay": { "deterministic": true, "idempotent": false }
  },
  "board-keeper.card.update": {
    "audit_assertion": [{ "operation": "card.update", "entity_type": "card" }],
    "replay": { "deterministic": true, "idempotent": true }
  },
  "board-keeper.card.status.change": {
    "audit_assertion": [{ "operation": "card.status.change", "entity_type": "card" }],
    "replay": { "deterministic": true, "idempotent": true }
  },
  "system.work.start": {
    "audit_assertion": [],
    "replay": { "deterministic": true, "idempotent": true }
  },
  "system.driver.tick": {
    "audit_assertion": [],
    "replay": { "deterministic": false, "idempotent": false }
  },
  "pl.session.start":              { "audit_assertion": [], "replay": { "deterministic": true,  "idempotent": false } },
  "pl.session.advance.clarify":    { "audit_assertion": [], "replay": { "deterministic": false, "idempotent": true  } },
  "pl.session.advance.research":   { "audit_assertion": [], "replay": { "deterministic": false, "idempotent": true  } },
  "pl.session.advance.decompose":  { "audit_assertion": [], "replay": { "deterministic": false, "idempotent": false } },
  "pl.session.finalize":           { "audit_assertion": [], "replay": { "deterministic": true,  "idempotent": false } },
  "pl.session.approve": {
    "audit_assertion": [{ "operation": "card.add", "entity_type": "card" }],
    "replay": { "deterministic": true, "idempotent": false }
  },
  "card.gate.pl.ratify": {
    "audit_assertion": [{ "operation": "card.gate.pl.ratify", "entity_type": "card" }],
    "replay": { "deterministic": true, "idempotent": true }
  },
  "contract.draft.auto": {
    "audit_assertion": [{ "operation": "contract.draft.auto", "entity_type": "card" }],
    "replay": { "deterministic": false, "idempotent": true }
  },
  "contract.gate.tl.sign": {
    "audit_assertion": [{ "operation": "contract.gate.tl.sign", "entity_type": "contract" }],
    "replay": { "deterministic": true, "idempotent": true }
  },
  "tl.sandbox.create": {
    "audit_assertion": [{ "operation": "contract.sandbox.create", "entity_type": "contract" }],
    "replay": { "deterministic": true, "idempotent": true }
  },
  "tl.refine.microtasks": {
    "audit_assertion": [{ "operation": "contract.refine.microtasks", "entity_type": "contract" }],
    "replay": { "deterministic": false, "idempotent": true }
  },
  "tl.plan.contract": {
    "audit_assertion": [{ "operation": "contract.plan.microtasks", "entity_type": "contract" }],
    "replay": { "deterministic": false, "idempotent": true }
  },
  "tl.author.snippets": {
    "audit_assertion": [{ "operation": "contract.author.snippets", "entity_type": "contract" }],
    "replay": { "deterministic": false, "idempotent": true }
  },
  "git-manager.implement.code":       { "audit_assertion": [], "replay": { "deterministic": true,  "idempotent": false } },
  "qa.gate.contract.verify":  { "audit_assertion": [], "replay": { "deterministic": false, "idempotent": true  } },
  "qa.acceptance.run":        { "audit_assertion": [], "replay": { "deterministic": true,  "idempotent": true  } },
  "man.card.review":          { "audit_assertion": [], "replay": { "deterministic": true,  "idempotent": true  } },
  "keeper.card.consolidate": {
    "audit_assertion": [{ "operation": "card.consolidate", "entity_type": "card" }],
    "replay": { "deterministic": false, "idempotent": true }
  },
  "keeper.journey.append":    { "audit_assertion": [], "replay": { "deterministic": true,  "idempotent": true  } }
}"#)
    .expect("CANONICAL_ACTION_SPECS: embedded JSON is valid by construction")
});

pub fn seedCanonical(db: &Connection) -> Value {
    let agent_sql = "INSERT OR IGNORE INTO agents (name, description, skills_json, context_json, permissions_json, model, parent_agent, specialization_md, applies_to_json, is_abstract) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";
    let agent_sub_update_sql = &format!("UPDATE agents SET parent_agent = ?, specialization_md = ?, applies_to_json = ?, is_abstract = ?, updated_at = {}
     WHERE name = ? AND (
       COALESCE(parent_agent,'') != COALESCE(?, '') OR
       COALESCE(specialization_md,'') != COALESCE(?, '') OR
       COALESCE(applies_to_json,'') != COALESCE(?, '') OR
       is_abstract != ?
     )", crate::clock::now_sql());
    for a in crate::engine_canonical_agents::CANONICAL_AGENTS.as_array().expect("array") {
        let parent_agent_raw = a.get("parent_agent");
        let specialization_md_raw = a.get("specialization_md");
        let applies_to_json_raw = a.get("applies_to_json");
        let is_abstract_raw = a.get("is_abstract");
        let parent_agent = nz(parent_agent_raw).cloned().unwrap_or(Value::Null);
        let specialization_md = nz(specialization_md_raw).cloned().unwrap_or(Value::Null);
        let applies_to_json = nz(applies_to_json_raw).cloned().unwrap_or(Value::Null);
        let is_abstract = nz(is_abstract_raw).cloned().unwrap_or(json!(0));
        let _ = crate::db_json::run_write(
            db,
            agent_sql,
            &[
                a["name"].clone(),
                a["description"].clone(),
                a["skills_json"].clone(),
                a["context_json"].clone(),
                a["permissions_json"].clone(),
                a["model"].clone(),
                parent_agent.clone(),
                specialization_md.clone(),
                applies_to_json.clone(),
                is_abstract.clone(),
            ],
        );
        if truthy(parent_agent_raw) || truthy(specialization_md_raw) || truthy(applies_to_json_raw) || truthy(is_abstract_raw) {
            let _ = crate::db_json::run_write(
                db,
                agent_sub_update_sql,
                &[
                    parent_agent.clone(),
                    specialization_md.clone(),
                    applies_to_json.clone(),
                    is_abstract.clone(),
                    a["name"].clone(),
                    parent_agent,
                    specialization_md,
                    applies_to_json,
                    is_abstract,
                ],
            );
        }
    }

    let skill_sql = "INSERT OR IGNORE INTO actions (name, description, owner_agent, steps_json, returns, on_failure) VALUES (?, ?, ?, ?, ?, ?)";
    let skill_update_sql = &format!("UPDATE actions SET steps_json = ?, description = ?, owner_agent = ?, returns = ?, on_failure = ?, updated_at = {}
     WHERE name = ? AND (
       steps_json != ? OR
       description != ? OR
       COALESCE(owner_agent,'') != COALESCE(?, '') OR
       COALESCE(returns,'')     != COALESCE(?, '') OR
       COALESCE(on_failure,'')  != COALESCE(?, '')
     )", crate::clock::now_sql());
    for s in crate::engine_canonical_actions::CANONICAL_ACTIONS.as_array().expect("array") {
        // NOT swallowed. A canonical action that fails to seed leaves its ROUTE registered and its
        // action absent, so the FE gets a 404 and renders "not wired — restart the BE" — an
        // instruction that cannot help, for a cause nothing reported. Seen for real: a substrate
        // sitting at 540 of ~666 actions with no indication anything had gone wrong.
        if let Err(e) = crate::db_json::run_write(
            db,
            skill_sql,
            &[
                s["name"].clone(),
                s["description"].clone(),
                s["owner_agent"].clone(),
                s["steps_json"].clone(),
                s["returns"].clone(),
                s["on_failure"].clone(),
            ],
        ) {
            eprintln!(
                "[seed] canonical action {} could not be inserted: {e}",
                s["name"].as_str().unwrap_or("<unnamed>")
            );
        }
        let _ = crate::db_json::run_write(
            db,
            skill_update_sql,
            &[
                s["steps_json"].clone(),
                s["description"].clone(),
                s["owner_agent"].clone(),
                s["returns"].clone(),
                s["on_failure"].clone(),
                s["name"].clone(),
                s["steps_json"].clone(),
                s["description"].clone(),
                s["owner_agent"].clone(),
                s["returns"].clone(),
                s["on_failure"].clone(),
            ],
        );
    }

    let spec_sql = "UPDATE actions SET verification_json = ? WHERE name = ? AND (verification_json IS NULL OR verification_json != ?)";
    if let Some(specs) = CANONICAL_ACTION_SPECS.as_object() {
        for (action_name, spec) in specs {
            let spec_str = serde_json::to_string(spec).unwrap_or_default();
            let _ = crate::db_json::run_write(db, spec_sql, &[json!(spec_str), json!(action_name), json!(spec_str)]);
        }
    }

    let grant_check_sql = "SELECT 1 FROM agent_actions WHERE agent_name = ? AND action_name = ? AND revoked_at IS NULL";
    let action_exists_sql = "SELECT 1 FROM actions WHERE name = ?";
    let grant_insert_sql = "INSERT INTO agent_actions (agent_name, action_name, granted_by) VALUES (?, ?, 'system:seed')";
    for a in crate::engine_canonical_agents::CANONICAL_AGENTS.as_array().expect("array") {
        let skills_json_str = a["skills_json"].as_str().unwrap_or("[]");
        let actions: Vec<Value> = serde_json::from_str(skills_json_str).unwrap_or_default();
        for action_name in actions {
            let has_grant = rows_nonempty(&crate::db_json::query_rows(db, grant_check_sql, &[a["name"].clone(), action_name.clone()]));
            if !has_grant {
                let exists = rows_nonempty(&crate::db_json::query_rows(db, action_exists_sql, &[action_name.clone()]));
                if exists {
                    let _ = crate::db_json::run_write(db, grant_insert_sql, &[a["name"].clone(), action_name.clone()]);
                }
            }
        }
    }

    seedCanonicalLaws(db);
    seedScoringRubric(db);
    seedAiProviders(db);
    seedRoutingDefaults(db);
    crate::engine_canonical_heimdall::seedHeimdall(db);
    seedAgentStages(db);
    seedWorkflows(db);
    seedEnvFlags(db);
    Value::Null
}

pub fn seedEnvFlags(db: &Connection) -> Value {
    let has = rows_nonempty(&crate::db_json::query_rows(
        db,
        "SELECT name FROM laws.sqlite_master WHERE type='table' AND name='env_flags'",
        &[],
    ));
    if !has {
        return Value::Null;
    }
    let sql = "
    INSERT INTO laws.env_flags (name, category, danger_tier, reads_at, default_value, description)
    VALUES (?, ?, ?, ?, ?, ?)
    ON CONFLICT(name) DO UPDATE SET
      category = excluded.category,
      danger_tier = excluded.danger_tier,
      reads_at = excluded.reads_at,
      default_value = excluded.default_value,
      description = excluded.description
  ";
    for row in crate::engine_canonical_env_flags::CANONICAL_ENV_FLAGS.as_array().expect("array") {
        let a = row.as_array().expect("tuple");
        let _ = crate::db_json::run_write(db, sql, &a.clone());
    }
    Value::Null
}

pub fn seedWorkflows(db: &Connection) -> Value {
    let has = rows_nonempty(&crate::db_json::query_rows(
        db,
        "SELECT name FROM sqlite_master WHERE type='table' AND name='workflows'",
        &[],
    ));
    if !has {
        return Value::Null;
    }
    let sql = "
    INSERT OR IGNORE INTO workflows (name, description, owner_agent, steps_json, on_failure)
    VALUES (?, ?, ?, ?, ?)
  ";
    let steps = json!([
        { "action": "keeper.review", "input": {}, "capture_as": "review" },
        { "action": "system.interviewer.session.consolidate", "input": { "session_id": "$.input.session_id" }, "capture_as": "consolidation" },
    ]);
    let steps_str = serde_json::to_string(&steps).unwrap_or_default();
    let _ = crate::db_json::run_write(
        db,
        sql,
        &[
            json!("workflow.session.close"),
            json!("Run keeper.review + extract patterns + auto-commit memories at session end. Wired to /end slash command + REPL exit + idle timeout."),
            json!("in"),
            json!(steps_str),
            json!("continue"),
        ],
    );
    Value::Null
}

pub fn seedAgentStages(db: &Connection) -> Value {
    let action_exists_sql = "SELECT 1 FROM actions WHERE name = ?";
    let cols = crate::db_json::query_rows(db, "PRAGMA table_info(agent_stages)", &[]).unwrap_or(json!([]));
    let has_sst = cols.as_array().map_or(false, |a| a.iter().any(|c| c["name"] == json!("pipeline_order")));

    let stage_insert_sst = "
        INSERT INTO agent_stages (agent_name, stage_name, order_index, description_md, entry_signal, exit_signal, pipeline_order, fe_group)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(agent_name, stage_name) DO UPDATE SET
          order_index=excluded.order_index, description_md=excluded.description_md,
          entry_signal=excluded.entry_signal, exit_signal=excluded.exit_signal,
          pipeline_order=excluded.pipeline_order, fe_group=excluded.fe_group
      ";
    let stage_insert_plain = "
        INSERT INTO agent_stages (agent_name, stage_name, order_index, description_md, entry_signal, exit_signal)
        VALUES (?, ?, ?, ?, ?, ?)
        ON CONFLICT(agent_name, stage_name) DO UPDATE SET
          order_index=excluded.order_index, description_md=excluded.description_md,
          entry_signal=excluded.entry_signal, exit_signal=excluded.exit_signal
      ";
    let stage_action_insert_sql = "
    INSERT OR IGNORE INTO agent_stage_actions (agent_stage_id, action_name, order_index, is_optional, invocation_pattern, rationale_md)
    VALUES (?, ?, ?, ?, ?, ?)
  ";
    let stage_id_sql = "SELECT id FROM agent_stages WHERE agent_name = ? AND stage_name = ?";

    for s in crate::engine_canonical_agent_stages::CANONICAL_AGENT_STAGES.as_array().expect("array") {
        let entry_signal = nz(s.get("entry_signal")).cloned().unwrap_or(Value::Null);
        let exit_signal = nz(s.get("exit_signal")).cloned().unwrap_or(Value::Null);
        if has_sst {
            let pipeline_order = nz(s.get("pipeline_order")).cloned().unwrap_or(Value::Null);
            let fe_group = nz(s.get("fe_group")).cloned().unwrap_or(Value::Null);
            let _ = crate::db_json::run_write(
                db,
                stage_insert_sst,
                &[
                    s["agent_name"].clone(),
                    s["stage_name"].clone(),
                    s["order_index"].clone(),
                    s["description_md"].clone(),
                    entry_signal,
                    exit_signal,
                    pipeline_order,
                    fe_group,
                ],
            );
        } else {
            let _ = crate::db_json::run_write(
                db,
                stage_insert_plain,
                &[
                    s["agent_name"].clone(),
                    s["stage_name"].clone(),
                    s["order_index"].clone(),
                    s["description_md"].clone(),
                    entry_signal,
                    exit_signal,
                ],
            );
        }
        let row = crate::db_json::query_rows(db, stage_id_sql, &[s["agent_name"].clone(), s["stage_name"].clone()]).unwrap_or(json!([]));
        let row_arr = row.as_array().cloned().unwrap_or_default();
        if row_arr.is_empty() {
            continue;
        }
        let stage_id = row_arr[0]["id"].clone();
        let mut i: i64 = 0;
        let empty: Vec<Value> = vec![];
        let actions = s.get("actions").and_then(|v| v.as_array()).unwrap_or(&empty);
        for sk in actions {
            let name_val = sk.get("action_name").cloned().unwrap_or(Value::Null);
            if !truthy(Some(&name_val)) {
                continue;
            }
            let exists = rows_nonempty(&crate::db_json::query_rows(db, action_exists_sql, &[name_val.clone()]));
            if !exists {
                continue;
            }
            let is_optional = truthy(sk.get("is_optional"));
            let invocation_pattern = nz(sk.get("invocation_pattern")).cloned().unwrap_or(json!("always"));
            let rationale_md = nz(sk.get("rationale_md")).cloned().unwrap_or(Value::Null);
            let _ = crate::db_json::run_write(
                db,
                stage_action_insert_sql,
                &[
                    stage_id.clone(),
                    name_val,
                    json!(i),
                    json!(if is_optional { 1 } else { 0 }),
                    invocation_pattern,
                    rationale_md,
                ],
            );
            i += 1;
        }
    }
    Value::Null
}

pub fn seedAiProviders(db: &Connection) -> Value {
    let provider_cols = crate::db_json::query_rows(db, "PRAGMA brain.table_info(ai_providers)", &[]).unwrap_or(json!([]));
    let provider_cols_arr = provider_cols.as_array().cloned().unwrap_or_default();
    if !provider_cols_arr.iter().any(|c| c["name"] == json!("failover_rank")) {
        let _ = db.execute("ALTER TABLE brain.ai_providers ADD COLUMN failover_rank INTEGER NOT NULL DEFAULT 0", []);
    }

    if let Ok(tick_cols_v) = crate::db_json::query_rows(db, "PRAGMA brain.table_info(driver_ticks)", &[]) {
        let tick_cols = tick_cols_v.as_array().cloned().unwrap_or_default();
        if !tick_cols.is_empty() && !tick_cols.iter().any(|c| c["name"] == json!("facts_json")) {
            let _ = db.execute("ALTER TABLE brain.driver_ticks ADD COLUMN facts_json TEXT", []);
        }
    }

    for t in ["build_gate_runs", "test_gate_runs", "lint_gate_runs"] {
        if let Ok(cols_v) = crate::db_json::query_rows(db, &format!("PRAGMA table_info({t})"), &[]) {
            let cols = cols_v.as_array().cloned().unwrap_or_default();
            if !cols.is_empty() {
                if !cols.iter().any(|c| c["name"] == json!("duration_ms")) {
                    let _ = db.execute(&format!("ALTER TABLE {t} ADD COLUMN duration_ms INTEGER"), []);
                }
                if (t == "test_gate_runs" || t == "build_gate_runs") && !cols.iter().any(|c| c["name"] == json!("phase")) {
                    let _ = db.execute(&format!("ALTER TABLE {t} ADD COLUMN phase TEXT NOT NULL DEFAULT 'after'"), []);
                }
            }
        }
    }

    if let Ok(tcols_v) = crate::db_json::query_rows(db, "PRAGMA table_info(tags)", &[]) {
        let tcols = tcols_v.as_array().cloned().unwrap_or_default();
        if !tcols.is_empty() {
            let has = |name: &str| tcols.iter().any(|c| c["name"] == json!(name));
            let add = |name: &str, ddl: &str| {
                if !has(name) {
                    let _ = db.execute(&format!("ALTER TABLE tags ADD COLUMN {ddl}"), []);
                }
            };
            add("applies_to_json", "applies_to_json TEXT");
            add("prevention_md", "prevention_md TEXT");
            add("retryability", "retryability TEXT");
            add("source_pattern_key", "source_pattern_key TEXT");
            add("fix_commit", "fix_commit TEXT");
            add("canary_test", "canary_test TEXT");
            add("weight", "weight REAL NOT NULL DEFAULT 1");
            add("status", "status TEXT NOT NULL DEFAULT 'open'");
        }
    }

    let has_adapter_module = provider_cols_arr.iter().any(|c| c["name"] == json!("adapter_module"));

    let openr = "antiloki-core/skills/external/openrouter-call.mjs";
    let anth = "antiloki-core/skills/external/anthropic-call.mjs";

    if has_adapter_module {
        let _ = db.execute(
            "UPDATE brain.ai_providers SET adapter_module = REPLACE(adapter_module, '.antiloki/skills/external/', 'antiloki-core/skills/external/') WHERE adapter_module LIKE '.antiloki/skills/external/%'",
            [],
        );
    }

    let stmt_full = "
        INSERT OR IGNORE INTO brain.ai_providers
          (tier, provider, model_id, active, cost_per_1m_input_tokens_usd,
           cost_per_1m_output_tokens_usd, max_context_tokens, notes_md, adapter_module)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
      ";
    let stmt_short = "
        INSERT OR IGNORE INTO brain.ai_providers
          (tier, provider, model_id, active, cost_per_1m_input_tokens_usd,
           cost_per_1m_output_tokens_usd, max_context_tokens, notes_md)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
      ";
    // THE SUBSCRIPTIONS FIRST (operator 2026-09-19): the coding CLIs the operator already pays for —
    // Claude Code, Codex, OpenCode — are providers on every tier; `cli/claude-code` is the default
    // (active, rank 0). OpenRouter is one option among them (BYOK: needs the key; seeded INACTIVE on a
    // fresh DB — an existing DB keeps its rows, and ai-call prefers cli among equal ranks anyway).
    // Cost columns are NULL for a subscription: the call is neither free nor metered per token.
    let cli_note = "your coding-CLI subscription, run headless (claude -p · codex exec · opencode run) — not metered per call";
    let rows: Vec<Vec<Value>> = vec![
        vec![json!("low"), json!("cli"), json!("claude-code"), json!(1), Value::Null, Value::Null, json!(200000), json!(cli_note), Value::Null],
        vec![json!("mid"), json!("cli"), json!("claude-code"), json!(1), Value::Null, Value::Null, json!(200000), json!(cli_note), Value::Null],
        vec![json!("high"), json!("cli"), json!("claude-code"), json!(1), Value::Null, Value::Null, json!(200000), json!(cli_note), Value::Null],
        vec![json!("low"), json!("cli"), json!("codex"), json!(0), Value::Null, Value::Null, json!(200000), json!(cli_note), Value::Null],
        vec![json!("mid"), json!("cli"), json!("codex"), json!(0), Value::Null, Value::Null, json!(200000), json!(cli_note), Value::Null],
        vec![json!("high"), json!("cli"), json!("codex"), json!(0), Value::Null, Value::Null, json!(200000), json!(cli_note), Value::Null],
        vec![json!("low"), json!("cli"), json!("opencode"), json!(0), Value::Null, Value::Null, json!(200000), json!(cli_note), Value::Null],
        vec![json!("mid"), json!("cli"), json!("opencode"), json!(0), Value::Null, Value::Null, json!(200000), json!(cli_note), Value::Null],
        vec![json!("high"), json!("cli"), json!("opencode"), json!(0), Value::Null, Value::Null, json!(200000), json!(cli_note), Value::Null],
        vec![json!("low"), json!("openrouter"), json!("qwen/qwen-2.5-coder-32b-instruct"), json!(0), json!(0.18), json!(0.18), json!(32000), json!("BYOK via OpenRouter; OPENROUTER_API_KEY (env or .antiloki/auth/openrouter.key)"), json!(openr)],
        vec![json!("mid"), json!("openrouter"), json!("anthropic/claude-sonnet-4.6"), json!(0), json!(3.00), json!(15.00), json!(200000), json!("BYOK via OpenRouter; OPENROUTER_API_KEY (env or .antiloki/auth/openrouter.key). NOTE: was anthropic/claude-3.5-sonnet until OpenRouter retired that id (404 No endpoints) — keep this a CURRENT valid id"), json!(openr)],
        vec![json!("high"), json!("openrouter"), json!("openai/gpt-5"), json!(0), json!(10.00), json!(40.00), json!(400000), json!("BYOK via OpenRouter; OPENROUTER_API_KEY (env or .antiloki/auth/openrouter.key)"), json!(openr)],
        vec![json!("low"), json!("anthropic"), json!("claude-haiku-4-5"), json!(0), json!(0.80), json!(4.00), json!(200000), json!("direct anthropic API; set active=1 if you have ANTHROPIC_API_KEY"), json!(anth)],
        vec![json!("mid"), json!("anthropic"), json!("claude-sonnet-4-6"), json!(0), json!(3.00), json!(15.00), json!(200000), json!("direct anthropic API; set active=1 if you have ANTHROPIC_API_KEY"), json!(anth)],
        vec![json!("high"), json!("anthropic"), json!("claude-opus-4-7"), json!(0), json!(15.0), json!(75.00), json!(1000000), json!("direct anthropic API; set active=1 if you have ANTHROPIC_API_KEY"), json!(anth)],
    ];
    for r in &rows {
        if has_adapter_module {
            let _ = crate::db_json::run_write(db, stmt_full, r);
        } else {
            let _ = crate::db_json::run_write(db, stmt_short, &r[0..8]);
        }
    }
    // EXISTING DBs (2026-09-19): the direct-anthropic rows are a JS-era adapter this runtime never
    // dispatches — off, so the status stops naming a provider that cannot answer. And a tier with no
    // dispatchable active row (cli · ollama · openrouter) gets the subscription default, so no tier
    // is ever silent.
    let _ = db.execute("UPDATE brain.ai_providers SET active = 0 WHERE provider = 'anthropic'", []);
    for tier in ["low", "mid", "high"] {
        let _ = db.execute(
            "UPDATE brain.ai_providers SET active = 1, failover_rank = 0 WHERE tier = ?1 AND provider = 'cli' AND model_id = 'claude-code' \
             AND NOT EXISTS (SELECT 1 FROM brain.ai_providers WHERE tier = ?1 AND active = 1 AND provider IN ('cli','ollama','openrouter'))",
            [tier],
        );
    }
    Value::Null
}

pub fn seedRoutingDefaults(db: &Connection) -> Value {
    let sql = "
    INSERT OR IGNORE INTO brain.routing_defaults
      (task_kind, tier, temperature, max_tokens, rationale_md)
    VALUES (?, ?, ?, ?, ?)
  ";
    let rows: Vec<Vec<Value>> = vec![
        vec![json!("pl.clarify.questions"), json!("mid"), json!("cold"), json!(1024), json!("needs reasoning + stable wording")],
        vec![json!("pl.research.summary"), json!("mid"), json!("cold"), json!(2048), json!("RAG context interpretation")],
        vec![json!("pl.decompose.decision"), json!("high"), json!("warm"), json!(2048), json!("hard call — depth + small variation to find creative splits")],
        vec![json!("pl.tune.concept"), json!("low"), json!("freezed"), json!(1024), json!("mechanical template + size + slug")],
        vec![json!("pl.reflect.rejection"), json!("low"), json!("freezed"), json!(512), json!("mechanical correction-memory composition")],
        vec![json!("contract.brainstorm.alternatives"), json!("high"), json!("hot"), json!(2048), json!("wants divergent options")],
        vec![json!("lint.feedback"), json!("low"), json!("freezed"), json!(512), json!("deterministic")],
    ];
    for r in &rows {
        let _ = crate::db_json::run_write(db, sql, r);
    }
    Value::Null
}

pub fn seedCanonicalLaws(db: &Connection) -> Value {
    let law_sql = "
    INSERT OR IGNORE INTO laws.laws
      (slug, kind, domain, title, statement, rationale, enforcement,
       enforcement_hook, trigger_sql, established_by, source_card)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
  ";
    let get_law_id_sql = "SELECT id FROM laws.laws WHERE slug = ?";
    let clause_exists_sql = "SELECT 1 FROM laws.law_clauses WHERE law_id = ? AND clause_type = ? AND text = ?";
    let clause_insert_sql = "INSERT INTO laws.law_clauses (law_id, clause_type, text, order_index) VALUES (?, ?, ?, ?)";

    for l in crate::engine_canonical_laws::CANONICAL_LAWS.as_array().expect("array") {
        let enforcement_hook = nz(l.get("enforcement_hook")).cloned().unwrap_or(Value::Null);
        let trigger_sql = nz(l.get("trigger_sql")).cloned().unwrap_or(Value::Null);
        let source_card = nz(l.get("source_card")).cloned().unwrap_or(Value::Null);
        let _ = crate::db_json::run_write(
            db,
            law_sql,
            &[
                l["slug"].clone(),
                l["kind"].clone(),
                l["domain"].clone(),
                l["title"].clone(),
                l["statement"].clone(),
                l["rationale"].clone(),
                l["enforcement"].clone(),
                enforcement_hook,
                trigger_sql,
                l["established_by"].clone(),
                source_card,
            ],
        );
        if let Some(clauses) = l.get("clauses").and_then(|v| v.as_array()) {
            if !clauses.is_empty() {
                let row = crate::db_json::query_rows(db, get_law_id_sql, &[l["slug"].clone()]).unwrap_or(json!([]));
                let row_arr = row.as_array().cloned().unwrap_or_default();
                if !row_arr.is_empty() {
                    let law_id = row_arr[0]["id"].clone();
                    for (i, c) in clauses.iter().enumerate() {
                        let order_index = nz(c.get("order_index")).cloned().unwrap_or(json!(i as i64));
                        let exists = rows_nonempty(&crate::db_json::query_rows(
                            db,
                            clause_exists_sql,
                            &[law_id.clone(), c["clause_type"].clone(), c["text"].clone()],
                        ));
                        if !exists {
                            let _ = crate::db_json::run_write(
                                db,
                                clause_insert_sql,
                                &[law_id.clone(), c["clause_type"].clone(), c["text"].clone(), order_index],
                            );
                        }
                    }
                }
            }
        }
    }
    Value::Null
}

pub fn seedScoringRubric(db: &Connection) -> Value {
    let has = rows_nonempty(&crate::db_json::query_rows(
        db,
        "SELECT name FROM laws.sqlite_master WHERE type='table' AND name='scoring_rubric'",
        &[],
    ));
    if !has {
        return Value::Null;
    }
    let seed: Vec<(&str, i64, &str)> = vec![
        ("risk", 1, "trivial, can't really break anything"),
        ("risk", 2, "low, isolated change"),
        ("risk", 3, "moderate, touches a shared surface"),
        ("risk", 4, "high, security/data integrity concern"),
        ("risk", 5, "critical, could brick prod / corrupt DB"),
        ("complexity", 1, "one-line fix"),
        ("complexity", 2, "small change, single file"),
        ("complexity", 3, "multi-file feature, well-scoped"),
        ("complexity", 4, "cross-cutting refactor or new subsystem"),
        ("complexity", 5, "epic; requires research/design phase"),
        ("priority", 1, "can sit, no one's waiting"),
        ("priority", 2, "nice to have"),
        ("priority", 3, "on the roadmap"),
        ("priority", 4, "blocking other work"),
        ("priority", 5, "drop everything, do now"),
    ];
    let sql = "INSERT OR IGNORE INTO laws.scoring_rubric (facet, level, definition) VALUES (?, ?, ?)";
    for (f, l, d) in &seed {
        let _ = crate::db_json::run_write(db, sql, &[json!(f), json!(l), json!(d)]);
    }
    Value::Null
}

pub fn applyCanonicalLawTriggers(db: &Connection) -> Value {
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT slug, trigger_sql FROM laws.laws
       WHERE superseded_by IS NULL AND trigger_sql IS NOT NULL AND trigger_sql != ''",
        &[],
    ) {
        Ok(v) => v,
        Err(_) => return Value::Null,
    };
    for r in rows.as_array().cloned().unwrap_or_default() {
        let trigger_sql = r["trigger_sql"].as_str().unwrap_or("");
        if let Err(e) = db.execute_batch(trigger_sql) {
            eprintln!("⚠ law trigger '{}' failed to apply: {e}", r["slug"].as_str().unwrap_or(""));
        }
    }
    Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS brain;").unwrap();
        db.execute_batch(
            "CREATE TABLE agents (
                id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, description TEXT NOT NULL,
                skills_json TEXT NOT NULL DEFAULT '[]', context_json TEXT NOT NULL DEFAULT '{}',
                permissions_json TEXT NOT NULL DEFAULT '{}', model TEXT, custom_prompt_md TEXT,
                parent_agent TEXT, specialization_md TEXT, applies_to_json TEXT, is_abstract INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT
            );
            CREATE TABLE actions (
                id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, description TEXT NOT NULL,
                owner_agent TEXT NOT NULL, steps_json TEXT NOT NULL, returns TEXT, on_failure TEXT NOT NULL DEFAULT 'abort',
                verification_json TEXT, kind TEXT DEFAULT 'atomic',
                updated_at TEXT
            );
            CREATE TABLE agent_actions (
                id INTEGER PRIMARY KEY AUTOINCREMENT, agent_name TEXT NOT NULL, action_name TEXT NOT NULL,
                granted_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), granted_by TEXT NOT NULL,
                conditions_json TEXT, revoked_at TEXT, revoked_by TEXT
            );
            CREATE TABLE workflows (
                id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, description TEXT NOT NULL,
                owner_agent TEXT NOT NULL, steps_json TEXT NOT NULL, on_failure TEXT NOT NULL DEFAULT 'abort',
                created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00'))
            );
            CREATE TABLE agent_stages (
                id INTEGER PRIMARY KEY AUTOINCREMENT, agent_name TEXT NOT NULL, stage_name TEXT NOT NULL,
                order_index INTEGER NOT NULL, description_md TEXT NOT NULL, entry_signal TEXT, exit_signal TEXT,
                pipeline_order INTEGER, fe_group TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
                UNIQUE (agent_name, stage_name)
            );
            CREATE TABLE agent_stage_actions (
                id INTEGER PRIMARY KEY AUTOINCREMENT, agent_stage_id INTEGER NOT NULL, action_name TEXT NOT NULL,
                order_index INTEGER NOT NULL, is_optional INTEGER NOT NULL DEFAULT 0, invocation_pattern TEXT NOT NULL,
                rationale_md TEXT, UNIQUE (agent_stage_id, action_name, order_index)
            );
            CREATE TABLE tags (
                slug TEXT PRIMARY KEY, description TEXT, kind TEXT NOT NULL DEFAULT 'theme',
                created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00'))
            );
            CREATE TABLE build_gate_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, passed INTEGER NOT NULL,
                skipped INTEGER NOT NULL DEFAULT 0, build_cmd TEXT, evidence_md TEXT, applied_at TEXT, snippets_hash TEXT,
                ts TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00'))
            );
            CREATE TABLE test_gate_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, passed INTEGER NOT NULL,
                skipped INTEGER DEFAULT 0, test_cmd TEXT, evidence_md TEXT, snippets_hash TEXT, duration_ms INTEGER,
                phase TEXT NOT NULL DEFAULT 'after', ts TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00'))
            );
            CREATE TABLE lint_gate_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, mode TEXT NOT NULL, blueprint_id INTEGER,
                version INTEGER, passed INTEGER NOT NULL, findings_json TEXT NOT NULL DEFAULT '[]', duration_ms INTEGER,
                ts TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00'))
            );
            CREATE TABLE laws.laws (
                id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, kind TEXT NOT NULL, domain TEXT NOT NULL,
                title TEXT NOT NULL, statement TEXT NOT NULL, rationale TEXT NOT NULL, enforcement TEXT NOT NULL,
                enforcement_hook TEXT, trigger_sql TEXT, established_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
                established_by TEXT NOT NULL, source_card TEXT, source_event INTEGER, source_memory INTEGER,
                superseded_by INTEGER, superseded_at TEXT
            );
            CREATE TABLE laws.law_clauses (
                id INTEGER PRIMARY KEY AUTOINCREMENT, law_id INTEGER NOT NULL, clause_type TEXT NOT NULL, text TEXT NOT NULL,
                order_index INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00'))
            );
            CREATE TABLE laws.scoring_rubric (
                id INTEGER PRIMARY KEY AUTOINCREMENT, facet TEXT NOT NULL, level INTEGER NOT NULL, definition TEXT NOT NULL,
                established_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), established_by TEXT NOT NULL DEFAULT 'migration',
                UNIQUE (facet, level)
            );
            CREATE TABLE laws.env_flags (
                name TEXT PRIMARY KEY, category TEXT NOT NULL, danger_tier TEXT NOT NULL, reads_at TEXT NOT NULL,
                default_value TEXT, description TEXT NOT NULL, added_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00'))
            );
            CREATE TABLE brain.ai_providers (
                id INTEGER PRIMARY KEY AUTOINCREMENT, tier TEXT NOT NULL, provider TEXT NOT NULL, model_id TEXT NOT NULL,
                active INTEGER NOT NULL DEFAULT 1, failover_rank INTEGER NOT NULL DEFAULT 0,
                cost_per_1m_input_tokens_usd REAL, cost_per_1m_output_tokens_usd REAL, max_context_tokens INTEGER,
                notes_md TEXT, adapter_module TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
                UNIQUE (tier, provider, model_id)
            );
            CREATE TABLE brain.routing_defaults (
                id INTEGER PRIMARY KEY AUTOINCREMENT, task_kind TEXT NOT NULL UNIQUE, tier TEXT NOT NULL,
                temperature TEXT NOT NULL, max_tokens INTEGER NOT NULL DEFAULT 1024, rationale_md TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00'))
            );
            CREATE TABLE brain.driver_ticks (
                id INTEGER PRIMARY KEY AUTOINCREMENT, started_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
                card_slug TEXT, decided_agent TEXT, decided_action TEXT, decided_reason TEXT, fired_event_id INTEGER,
                outcome TEXT NOT NULL DEFAULT 'running', cost_usd REAL DEFAULT 0, duration_ms INTEGER, ended_at TEXT,
                error_message TEXT
            );
            CREATE TABLE brain.heimdall_weight_profiles (slug TEXT PRIMARY KEY, title TEXT NOT NULL, weights_json TEXT NOT NULL);
            CREATE TABLE brain.heimdall_categories (slug TEXT PRIMARY KEY, family TEXT NOT NULL, title TEXT NOT NULL, description_md TEXT NOT NULL DEFAULT '', weight_profile TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'dormant', graded_by TEXT NOT NULL DEFAULT 'both');
            CREATE TABLE brain.heimdall_template_categories (template_base TEXT PRIMARY KEY, category TEXT NOT NULL);",
        )
        .unwrap();
        db
    }

    #[test]
    fn seed_canonical_populates_agents_actions_and_specs() {
        let db = base_db();
        assert_eq!(seedCanonical(&db), Value::Null);
        let agent_count: i64 = db.query_row("SELECT COUNT(*) FROM agents", [], |r| r.get(0)).unwrap();
        assert!(agent_count > 0);
        let action_count: i64 = db.query_row("SELECT COUNT(*) FROM actions", [], |r| r.get(0)).unwrap();
        assert!(action_count > 0);
        let spec_count: i64 = db.query_row("SELECT COUNT(*) FROM actions WHERE verification_json IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(spec_count as usize, CANONICAL_ACTION_SPECS.as_object().unwrap().len());
        let heimdall_profiles: i64 = db.query_row("SELECT COUNT(*) FROM brain.heimdall_weight_profiles", [], |r| r.get(0)).unwrap();
        assert_eq!(heimdall_profiles, 4);
    }

    #[test]
    fn seed_env_flags_no_op_when_table_absent() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("ATTACH ':memory:' AS laws;").unwrap();
        assert_eq!(seedEnvFlags(&db), Value::Null);
    }

    #[test]
    fn seed_env_flags_populates_and_upserts() {
        let db = base_db();
        assert_eq!(seedEnvFlags(&db), Value::Null);
        let count: i64 = db.query_row("SELECT COUNT(*) FROM laws.env_flags", [], |r| r.get(0)).unwrap();
        assert_eq!(count as usize, crate::engine_canonical_env_flags::CANONICAL_ENV_FLAGS.as_array().unwrap().len());
    }

    #[test]
    fn seed_workflows_inserts_session_close() {
        let db = base_db();
        assert_eq!(seedWorkflows(&db), Value::Null);
        let name: String = db.query_row("SELECT name FROM workflows", [], |r| r.get(0)).unwrap();
        assert_eq!(name, "workflow.session.close");
    }

    #[test]
    fn seed_agent_stages_no_op_without_matching_actions() {
        let db = base_db();
        // no actions seeded, so agent_stage_actions rows should never be inserted
        assert_eq!(seedAgentStages(&db), Value::Null);
        let stages: i64 = db.query_row("SELECT COUNT(*) FROM agent_stages", [], |r| r.get(0)).unwrap();
        assert!(stages > 0);
        let stage_actions: i64 = db.query_row("SELECT COUNT(*) FROM agent_stage_actions", [], |r| r.get(0)).unwrap();
        assert_eq!(stage_actions, 0);
    }

    #[test]
    fn seed_ai_providers_self_heals_missing_columns_and_seeds_rows() {
        let db = base_db();
        assert_eq!(seedAiProviders(&db), Value::Null);
        let count: i64 = db.query_row("SELECT COUNT(*) FROM brain.ai_providers", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 15); // 9 cli (3 engines × 3 tiers) + 3 openrouter + 3 anthropic
        // the default of every tier is the Claude Code subscription; OpenRouter is seeded inactive (BYOK opt-in)
        for tier in ["low", "mid", "high"] {
            let (p, m): (String, String) = db.query_row("SELECT provider, model_id FROM brain.ai_providers WHERE tier = ?1 AND active = 1 ORDER BY failover_rank, id LIMIT 1", [tier], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
            assert_eq!((p.as_str(), m.as_str()), ("cli", "claude-code"), "{tier}");
        }
        let or_active: i64 = db.query_row("SELECT COUNT(*) FROM brain.ai_providers WHERE provider = 'openrouter' AND active = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(or_active, 0);
        let build_cols: Vec<String> = db
            .prepare("SELECT name FROM pragma_table_info('build_gate_runs')")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(build_cols.contains(&"duration_ms".to_string()));
        assert!(build_cols.contains(&"phase".to_string()));
        let tag_cols: Vec<String> = db
            .prepare("SELECT name FROM pragma_table_info('tags')")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(tag_cols.contains(&"weight".to_string()));
    }

    #[test]
    fn seed_routing_defaults_inserts_seven_rows() {
        let db = base_db();
        assert_eq!(seedRoutingDefaults(&db), Value::Null);
        let count: i64 = db.query_row("SELECT COUNT(*) FROM brain.routing_defaults", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 7);
    }

    #[test]
    fn seed_canonical_laws_inserts_laws_and_clauses() {
        let db = base_db();
        assert_eq!(seedCanonicalLaws(&db), Value::Null);
        let count: i64 = db.query_row("SELECT COUNT(*) FROM laws.laws", [], |r| r.get(0)).unwrap();
        assert_eq!(count as usize, crate::engine_canonical_laws::CANONICAL_LAWS.as_array().unwrap().len());
    }

    #[test]
    fn seed_scoring_rubric_inserts_fifteen_rows() {
        let db = base_db();
        assert_eq!(seedScoringRubric(&db), Value::Null);
        let count: i64 = db.query_row("SELECT COUNT(*) FROM laws.scoring_rubric", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 15);
    }

    #[test]
    fn apply_canonical_law_triggers_runs_stored_trigger_sql() {
        let db = base_db();
        db.execute(
            "INSERT INTO laws.laws (slug, kind, domain, title, statement, rationale, enforcement, trigger_sql, established_by)
             VALUES ('T', 'constraint', 'audit', 'T', 'T', 'T', 'hard', 'CREATE TABLE laws.t_trigger_marker (id INTEGER);', 'system')",
            [],
        )
        .unwrap();
        assert_eq!(applyCanonicalLawTriggers(&db), Value::Null);
        let exists: i64 = db
            .query_row("SELECT COUNT(*) FROM laws.sqlite_master WHERE name='t_trigger_marker'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(exists, 1);
    }

    #[test]
    fn apply_canonical_law_triggers_no_op_when_laws_table_absent() {
        let db = Connection::open_in_memory().unwrap();
        assert_eq!(applyCanonicalLawTriggers(&db), Value::Null);
    }
}
