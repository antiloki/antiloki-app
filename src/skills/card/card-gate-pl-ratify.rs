//! card-gate-pl-ratify.rs — twin of skills/card/card-gate-pl-ratify.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `cardGatePlRatify` (default).
//!
//! Cross-module: learnInsight (skills/ai/learn-insight.mjs, CONVERTED — crate::ai_learn_insight).
//! Fixture only exercises the success paths (accept + reject); the JS source's various
//! AntilokiError throws (contract not found, wrong status, missing missing_fields[], learn-insight
//! failure) are reproduced as Err(String) with matching messages but are not fixture-compared
//! (GOTCHA 9 — thrown-error messages are fragile / out of scope for conform).
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
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
fn str_or(v: Option<&Value>, default: &str) -> String {
    match nz(v) {
        Some(Value::String(s)) => s.clone(),
        Some(x) => x.to_string(),
        None => default.to_string(),
    }
}

pub fn cardGatePlRatify(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));

    let contract_slug = get("contract_slug").cloned().unwrap_or(Value::Null);
    let decision = get("decision").cloned().unwrap_or(Value::Null);
    let missing_fields = match get("missing_fields") {
        None => json!([]),
        Some(v) => v.clone(),
    };
    let reason = get("reason").cloned().unwrap_or(Value::Null);
    let signed_by = match nz(get("signed_by")) {
        Some(v) => v.clone(),
        None => json!("agent:pl"),
    };

    if !truthy(Some(&contract_slug)) {
        return Err("card-gate-pl-ratify: contract_slug is required".into());
    }
    if !truthy(Some(&decision)) {
        return Err("card-gate-pl-ratify: decision is required".into());
    }
    let decision_s = decision.as_str().unwrap_or("");
    if decision_s != "accept" && decision_s != "reject" {
        return Err(format!(
            "card-gate-pl-ratify: decision must be one of: accept, reject (got '{}')",
            decision_s
        ));
    }

    let rows = crate::db_json::query_rows(db, "SELECT * FROM contracts WHERE slug = ?", &[contract_slug.clone()])?;
    let contract = match rows.as_array().and_then(|a| a.first()) {
        Some(c) => c.clone(),
        None => {
            return Err(format!(
                "card-gate-pl-ratify: contract '{}' not found",
                contract_slug.as_str().unwrap_or_default()
            ))
        }
    };

    if decision_s == "accept" {
        let status = contract.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if status != "proposed" {
            return Err(format!(
                "card-gate-pl-ratify: contract status is '{}', expected 'proposed' for PL ratification",
                status
            ));
        }
        crate::db_json::run_write(
            db,
            &format!("UPDATE contracts SET status = 'pl_ratified', updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
            &[contract_slug.clone()],
        )?;
        let mut out = Map::new();
        out.insert("ratified".into(), json!(true));
        out.insert("contract_slug".into(), contract_slug.clone());
        out.insert("new_status".into(), json!("pl_ratified"));
        out.insert("ratified_by".into(), signed_by.clone());
        out.insert("ratified_at".into(), json!(crate::clock::now_iso_nodb()));
        return Ok(Value::Object(out));
    }

    // reject branch
    let missing_arr = missing_fields.as_array().cloned().unwrap_or_default();
    if missing_arr.is_empty() {
        return Err("card-gate-pl-ratify: reject requires missing_fields[] (at least one)".into());
    }

    let template_kind = str_or(contract.get("template_kind"), "none");
    let missing_joined = missing_arr
        .iter()
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let contract_slug_s = contract_slug.as_str().unwrap_or_default();
    let reason_suffix = if truthy(Some(&reason)) {
        format!(" Reason: {}", reason.as_str().unwrap_or_default())
    } else {
        String::new()
    };
    let guidance = format!(
        "PL rejected contract '{}' (template={}). Missing/wrong: {}.{}",
        contract_slug_s, template_kind, missing_joined, reason_suffix
    );

    let card_slug = str_or(contract.get("card_slug"), "?");
    let situation_md = format!(
        "contract drafted for card '{}' with template_kind='{}'",
        card_slug, template_kind
    );

    let mut naive_obj = Map::new();
    naive_obj.insert("slug".into(), contract.get("slug").cloned().unwrap_or(Value::Null));
    naive_obj.insert("files_touched".into(), contract.get("files_touched_json").cloned().unwrap_or(Value::Null));
    naive_obj.insert("template_kind".into(), contract.get("template_kind").cloned().unwrap_or(Value::Null));
    naive_obj.insert("tier".into(), contract.get("tier").cloned().unwrap_or(Value::Null));
    let naive_md = take_chars(
        &serde_json::to_string(&Value::Object(naive_obj)).unwrap_or_default(),
        800,
    );

    let tags = json!([
        "card-ratification",
        "pl-rejection",
        "contract",
        str_or(contract.get("template_kind"), "no-template")
    ]);

    let insight = crate::ai_learn_insight::learnInsight(
        &json!({
            "moment_kind": "rejection",
            "emitter": "card.gate.pl.ratify",
            "source_ref": contract_slug,
            "situation_md": situation_md,
            "naive_md": naive_md,
            "corrected_md": guidance,
            "tags": tags,
            "created_by": signed_by,
        }),
        db,
    )
    .map_err(|e| format!("card-gate-pl-ratify: failed to record learning insight: {e}"))?;

    let memory_id = nz(insight.get("memory_id")).cloned().unwrap_or(Value::Null);

    let mut out = Map::new();
    out.insert("ratified".into(), json!(false));
    out.insert("contract_slug".into(), contract_slug);
    out.insert("missing_fields".into(), missing_fields);
    out.insert("reason".into(), reason);
    out.insert("memory_id".into(), memory_id);
    out.insert("insight_path".into(), insight.get("insight_path").cloned().unwrap_or(json!([])));
    out.insert("links_added".into(), insight.get("links_added").cloned().unwrap_or(json!([])));
    out.insert(
        "consolidation_triggered".into(),
        insight.get("consolidation_triggered").cloned().unwrap_or(json!(false)),
    );
    out.insert(
        "self_learning".into(),
        json!("recorded via system.learn.insight; RAG-auto-linked to related"),
    );
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
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              slug TEXT NOT NULL UNIQUE,
              title TEXT NOT NULL,
              status TEXT NOT NULL DEFAULT 'proposed',
              template_kind TEXT,
              size TEXT,
              card_slug TEXT,
              files_touched_json TEXT NOT NULL DEFAULT '[]',
              tier TEXT,
              updated_at TEXT
            );
            CREATE TABLE memory.catalog_embeddings (entity_type TEXT NOT NULL, entity_name TEXT NOT NULL, model TEXT NOT NULL, dims INTEGER NOT NULL, vector BLOB NOT NULL);
            CREATE TABLE memory.memories (
              id              INTEGER PRIMARY KEY AUTOINCREMENT,
              type            TEXT    NOT NULL,
              title           TEXT    NOT NULL,
              situation       TEXT    NOT NULL,
              guidance        TEXT    NOT NULL,
              naive_response  TEXT,
              rationale       TEXT,
              tags_json       TEXT    NOT NULL DEFAULT '[]',
              source_card     TEXT,
              source_event    INTEGER,
              source_session  TEXT,
              confidence      INTEGER NOT NULL DEFAULT 0,
              validated       INTEGER NOT NULL DEFAULT 0,
              superseded_by   INTEGER,
              agent_slug      TEXT,
              created_at      TEXT    NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
              created_by      TEXT    NOT NULL
            );
            CREATE TABLE memory.memory_links (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              from_memory_id INTEGER NOT NULL,
              to_memory_id INTEGER NOT NULL,
              kind TEXT NOT NULL,
              rationale TEXT,
              weight REAL NOT NULL DEFAULT 1.0,
              created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
              created_by TEXT NOT NULL
            );",
        )
        .unwrap();
        c
    }

    #[test]
    fn accept_advances_status() {
        let c = db();
        c.execute(
            "INSERT INTO contracts (slug, title, status) VALUES ('CONTRACT-1', 'test', 'proposed')",
            [],
        )
        .unwrap();
        let r = cardGatePlRatify(
            &json!({ "contract_slug": "CONTRACT-1", "decision": "accept", "signed_by": "agent:pl" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["ratified"], json!(true));
        assert_eq!(r["new_status"], json!("pl_ratified"));
        assert_eq!(r["ratified_by"], json!("agent:pl"));
        let status: String = c
            .query_row("SELECT status FROM contracts WHERE slug='CONTRACT-1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(status, "pl_ratified");
    }

    #[test]
    fn reject_writes_correction_memory() {
        let c = db();
        c.execute(
            "INSERT INTO contracts (slug, title, status, template_kind, card_slug, files_touched_json, tier) VALUES ('CONTRACT-1', 'test', 'proposed', 'feature-add', 'CARD-1', '[\"x.mjs\"]', 'standard')",
            [],
        )
        .unwrap();
        let r = cardGatePlRatify(
            &json!({ "contract_slug": "CONTRACT-1", "decision": "reject",
                "missing_fields": ["wrong scope", "card-X intent drifted"], "reason": "expanded beyond card" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["ratified"], json!(false));
        assert!(r["memory_id"].is_number());
        let status: String = c
            .query_row("SELECT status FROM contracts WHERE slug='CONTRACT-1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(status, "proposed");
        let tags: String = c
            .query_row("SELECT tags_json FROM memory.memories WHERE id = ?", [r["memory_id"].as_i64().unwrap()], |row| row.get(0))
            .unwrap();
        assert!(tags.contains("card-ratification"));
    }
}
