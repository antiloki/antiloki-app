//! blueprint/tl-contract-assemble.rs — twin of skills/blueprint/tl-contract-assemble.mjs (db lane,
//! default export `tlContractAssemble`). Deterministically assembles the CONTRACT (pipeline v2 PLAN
//! step 3) from the card + PL screening: no AI, just mechanical parts assembly + a scope-gravity cap
//! + reassembly-after-rejection nudges. Cross-module calls (both already-converted twins in this
//! batch): `crate::blueprint_contract_lint::contractLint` (deterministic gate on the assembled
//! fields) and `crate::system_naming_derive::namingDerive` (naming authority, best-effort — JS
//! wraps the call in try/catch and swallows ANY failure: "naming is additive — never blocks
//! assembly"; ported here as `let _ = ...`, ignoring the Result the same way). namingDerive's own
//! cross-module dependency (`getWorkspaceRoot` -> `appPrefix` -> a real `readFileSync`) resolves to
//! the SAME real repo directory in both the JS and Rust conform lanes (per workspace-root.rs's
//! documented CARGO_MANIFEST_DIR-parent == antiloki root convention) and that directory has no
//! `package.json`, so `appPrefix` deterministically falls back to `"app"` on both sides — the cross-
//! module call is safe to make unconditionally rather than blocking this module on it.
//!
//! Frozen-clock lane: the sole `datetime('now')` (the reassembly supersede UPDATE) ->
//! `datetime('2026-01-01 00:00:00')`.
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
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

const CORE_MUST_OBEY: [&str; 3] = ["NEVER_BYPASS_WITH_AUDIT", "HISTORY_IS_SACRED_NO_REVERT", "COMMIT_CODE_BEFORE_AUDIT_COMMIT"];

pub fn tlContractAssemble(input: &Value, db: &Connection) -> Result<Value, String> {
    let card_slug = input.get("card_slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let card_slug = match card_slug {
        Some(s) => s,
        None => return Err("tl-contract-assemble: card_slug is required".to_string()),
    };

    let card_rows = crate::db_json::query_rows(db, "SELECT slug, title, body_json FROM cards WHERE slug = ?", &[json!(card_slug)])?;
    let card = match card_rows.as_array().and_then(|a| a.first()) {
        Some(c) => c.clone(),
        None => return Err(format!("tl-contract-assemble: card '{card_slug}' not found")),
    };

    let screening_id = input.get("screening_id");
    let scr_rows = if truthy(screening_id) {
        crate::db_json::query_rows(db, "SELECT id, screening_json FROM pl_screenings WHERE id = ?", &[screening_id.cloned().unwrap()])?
    } else {
        crate::db_json::query_rows(db, "SELECT id, screening_json FROM pl_screenings WHERE card_slug = ? ORDER BY id DESC LIMIT 1", &[json!(card_slug)])?
    };
    let scr = match scr_rows.as_array().and_then(|a| a.first()) {
        Some(s) => s.clone(),
        None => return Err(format!("tl-contract-assemble: screening '{card_slug}' not found")),
    };

    let screening: Value = scr
        .get("screening_json")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or_else(|| json!({}));

    let mut acceptance: Vec<Value> = screening.get("acceptance").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut out_of_scope: Vec<Value> = screening.get("out_of_scope").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let cm = screening.get("context_md");
    let cm_trimmed = if truthy(cm) { str_qq(cm).trim().to_string() } else { String::new() };
    let mut goal_md = if !cm_trimmed.is_empty() {
        cm_trimmed
    } else if truthy(card.get("title")) {
        str_qq(card.get("title"))
    } else {
        format!("deliver {card_slug}")
    };

    let prior_rows = crate::db_json::query_rows(
        db,
        "SELECT slug, status, failure_kind, rejection_reason_md FROM contracts WHERE card_slug = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1",
        &[json!(card_slug)],
    )?;
    let prior = prior_rows.as_array().and_then(|a| a.first()).cloned();
    let reassembling = prior.as_ref().map_or(false, |p| {
        p.get("status").and_then(|v| v.as_str()) == Some("rejected") && p.get("failure_kind").and_then(|v| v.as_str()) == Some("pl_ratify_rejected")
    });
    let prior_rejects_rows = crate::db_json::query_rows(
        db,
        "SELECT COUNT(*) AS n FROM contracts WHERE card_slug = ? AND failure_kind = 'pl_ratify_rejected'",
        &[json!(card_slug)],
    )?;
    let prior_rejects: i64 = prior_rejects_rows
        .as_array()
        .and_then(|a| a.first())
        .and_then(|r| r.get("n"))
        .and_then(Value::as_i64)
        .unwrap_or(0);

    if reassembling {
        let reason_v = prior.as_ref().and_then(|p| p.get("rejection_reason_md"));
        let reason_full = match nz(reason_v) {
            Some(v) => str_qq(Some(v)),
            None => "(no reason recorded)".to_string(),
        };
        let truncated: String = reason_full.chars().take(1200).collect();
        goal_md.push_str(&format!("\n\n\u{2550}\u{2550}\u{2550} PRIOR RATIFY REJECTION \u{2014} ADDRESS EXPLICITLY \u{2550}\u{2550}\u{2550}\n{truncated}"));
    }

    let body: Value = card
        .get("body_json")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or_else(|| json!({}));
    let card_size = match nz(body.get("size")) {
        Some(v) => str_qq(Some(v)).to_uppercase(),
        None => "M".to_string(),
    };
    let tiers = ["XS", "S", "M", "L"];
    let base_idx = tiers.iter().position(|t| *t == card_size).unwrap_or(2);
    let eff_idx = (base_idx + prior_rejects.max(0) as usize).min(tiers.len() - 1);
    let eff_size = tiers[eff_idx];
    let cap: i64 = match eff_size {
        "XS" => 6,
        "S" => 8,
        "M" => 12,
        "L" => 20,
        _ => 12,
    };

    let mut deferred_count: i64 = 0;
    if (acceptance.len() as i64) > cap {
        let n = acceptance.len();
        let mut group_order: Vec<String> = Vec::new();
        let mut group_first_idx: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for (i, a) in acceptance.iter().enumerate() {
            let k = match a.get("card_acceptance_id") {
                Some(v) if !v.is_null() => v.clone(),
                _ => json!("_"),
            };
            let ks = serde_json::to_string(&k).unwrap_or_default();
            if !group_first_idx.contains_key(&ks) {
                group_first_idx.insert(ks.clone(), i);
                group_order.push(ks);
            }
        }
        let mut selected: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for ks in &group_order {
            if let Some(&idx) = group_first_idx.get(ks) {
                selected.insert(idx);
            }
        }
        for i in 0..n {
            if (selected.len() as i64) >= cap {
                break;
            }
            selected.insert(i);
        }
        let mut kept: Vec<Value> = Vec::new();
        let mut deferred: Vec<Value> = Vec::new();
        for (i, a) in acceptance.iter().enumerate() {
            if selected.contains(&i) {
                kept.push(a.clone());
            } else {
                deferred.push(a.clone());
            }
        }
        deferred_count = deferred.len() as i64;
        for a in &deferred {
            let lbl = nz(a.get("intent")).or_else(|| nz(a.get("id")));
            let s = str_qq(lbl);
            let truncated: String = s.chars().take(140).collect();
            out_of_scope.push(json!(format!("deferred acceptance (size {eff_size} cap {cap}): {truncated}")));
        }
        acceptance = kept;
    }

    if reassembling {
        let has_reason = prior.as_ref().map_or(false, |p| truthy(p.get("rejection_reason_md")));
        if has_reason {
            let reason_v = prior.as_ref().and_then(|p| p.get("rejection_reason_md"));
            let reason_full = str_qq(reason_v);
            let rej: String = reason_full.chars().take(400).collect();
            let rej_short: String = rej.chars().take(140).collect();
            let mut item = Map::new();
            item.insert("id".into(), json!(format!("ratify-demand-r{prior_rejects}")));
            item.insert("card_acceptance_id".into(), json!("ratify-demand"));
            item.insert("intent".into(), json!(format!("satisfy the prior ratify rejection (attempt {prior_rejects}): {rej_short}")));
            item.insert("done_criteria".into(), json!(rej));
            item.insert("kind".into(), json!("behavior"));
            acceptance.push(Value::Object(item));
        }
    }

    let acceptance_json = serde_json::to_string(&acceptance).unwrap_or_else(|_| "[]".to_string());
    let must_obey_json = serde_json::to_string(&CORE_MUST_OBEY).unwrap_or_else(|_| "[]".to_string());
    let files_touched_json = "[]".to_string();
    let out_of_scope_json = serde_json::to_string(&out_of_scope).unwrap_or_else(|_| "[]".to_string());

    let mut contract_fields = Map::new();
    contract_fields.insert("goal_md".into(), json!(goal_md));
    contract_fields.insert("acceptance_json".into(), json!(acceptance_json));
    contract_fields.insert("must_obey_json".into(), json!(must_obey_json));
    contract_fields.insert("files_touched_json".into(), json!(files_touched_json));
    contract_fields.insert("out_of_scope_json".into(), json!(out_of_scope_json));

    let lint = crate::blueprint_contract_lint::contractLint(&json!({ "contract": Value::Object(contract_fields) }), db)?;
    let passed = lint.get("passed").cloned().unwrap_or(Value::Bool(false));

    let prev_rows = crate::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM contracts WHERE card_slug = ?", &[json!(card_slug)])?;
    let prev: i64 = prev_rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).and_then(Value::as_i64).unwrap_or(0);
    let version = prev + 1;
    let slug = format!("{card_slug}-CONTRACT-{version}");

    let title = match nz(card.get("title")) {
        Some(v) => str_qq(Some(v)),
        None => slug.clone(),
    };

    crate::db_json::run_write(
        db,
        "INSERT INTO contracts (slug, title, card_slug, status, goal_md, acceptance_json, must_obey_json, files_touched_json, out_of_scope_json, naming_json, edge_cases_json, reference_artifacts_json, current_attempt) VALUES (?, ?, ?, 'proposed', ?, ?, ?, ?, ?, '{}', '[]', '[]', ?)",
        &[
            json!(slug),
            json!(title),
            json!(card_slug),
            json!(goal_md),
            json!(acceptance_json),
            json!(must_obey_json),
            json!(files_touched_json),
            json!(out_of_scope_json),
            json!(version),
        ],
    )?;

    if reassembling {
        let prior_slug = prior.as_ref().and_then(|p| p.get("slug")).cloned().unwrap_or(Value::Null);
        crate::db_json::run_write(
            db,
            &format!("UPDATE contracts SET status = 'superseded', superseded_by = ?, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
            &[json!(slug), prior_slug],
        )?;
    }

    // naming is additive — never blocks assembly (JS: try { namingDerive(...) } catch {}).
    let _ = crate::system_naming_derive::namingDerive(&json!({ "contract_slug": slug }), db);

    let mut out = Map::new();
    out.insert("contract_slug".into(), json!(slug));
    out.insert("screening_id".into(), scr.get("id").cloned().unwrap_or(Value::Null));
    out.insert("lint".into(), lint);
    out.insert("passed".into(), passed);
    out.insert("acceptance_kept".into(), json!(acceptance.len() as i64));
    out.insert("acceptance_deferred".into(), json!(deferred_count));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, title TEXT NOT NULL, body_json TEXT);
             CREATE TABLE pl_screenings (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT NOT NULL, screening_json TEXT);
             CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, title TEXT, card_slug TEXT,
               status TEXT NOT NULL DEFAULT 'proposed', goal_md TEXT, acceptance_json TEXT NOT NULL DEFAULT '[]',
               must_obey_json TEXT NOT NULL DEFAULT '[]', files_touched_json TEXT NOT NULL DEFAULT '[]',
               out_of_scope_json TEXT NOT NULL DEFAULT '[]', naming_json TEXT NOT NULL DEFAULT '{}',
               edge_cases_json TEXT NOT NULL DEFAULT '[]', reference_artifacts_json TEXT NOT NULL DEFAULT '[]',
               current_attempt INTEGER, failure_kind TEXT, rejection_reason_md TEXT, superseded_by TEXT, updated_at TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn assembles_contract_from_card_and_screening() {
        let db = make_db();
        db.execute("INSERT INTO cards (slug, title, body_json) VALUES ('CARD-A', 'add a strict filter', '{\"size\":\"S\"}')", []).unwrap();
        db.execute(
            "INSERT INTO pl_screenings (card_slug, screening_json) VALUES ('CARD-A', ?1)",
            rusqlite::params![serde_json::to_string(&json!({
                "context_md": "filter users to males over 30, strictly typed",
                "acceptance": [{"id":"ac1","intent":"filter males>30","done_criteria":"only m & >30","kind":"behavior"}]
            })).unwrap()],
        )
        .unwrap();
        let r = tlContractAssemble(&json!({ "card_slug": "CARD-A" }), &db).unwrap();
        assert_eq!(r["passed"], json!(true));
        let rows = crate::db_json::query_rows(&db, "SELECT status, goal_md, acceptance_json, files_touched_json FROM contracts WHERE slug = ?1", &[r["contract_slug"].clone()]).unwrap();
        let c = &rows[0];
        assert_eq!(c["status"], json!("proposed"));
        assert!(c["goal_md"].as_str().unwrap().contains("males over 30"));
        assert_eq!(c["files_touched_json"], json!("[]"));
    }

    #[test]
    fn missing_card_slug_errors() {
        let db = make_db();
        let r = tlContractAssemble(&json!({}), &db);
        assert_eq!(r, Err("tl-contract-assemble: card_slug is required".to_string()));
    }

    #[test]
    fn cap_defers_overflow_acceptance_by_size() {
        let db = make_db();
        db.execute("INSERT INTO cards (slug, title, body_json) VALUES ('CARD-B', 'big card', '{\"size\":\"XS\"}')", []).unwrap();
        let items: Vec<Value> = (0..8)
            .map(|i| json!({"id": format!("ac{i}"), "card_acceptance_id": format!("g{}", i % 3), "intent": format!("do thing {i}"), "done_criteria": "d", "kind": "behavior"}))
            .collect();
        db.execute(
            "INSERT INTO pl_screenings (card_slug, screening_json) VALUES ('CARD-B', ?1)",
            rusqlite::params![serde_json::to_string(&json!({"context_md": "big feature", "acceptance": items})).unwrap()],
        )
        .unwrap();
        let r = tlContractAssemble(&json!({ "card_slug": "CARD-B" }), &db).unwrap();
        assert_eq!(r["acceptance_kept"], json!(6));
        assert_eq!(r["acceptance_deferred"], json!(2));
    }

    #[test]
    fn reassembly_after_rejection_adds_ratify_demand_and_supersedes_prior() {
        let db = make_db();
        db.execute("INSERT INTO cards (slug, title, body_json) VALUES ('CARD-C', 'retry card', '{\"size\":\"S\"}')", []).unwrap();
        db.execute(
            "INSERT INTO pl_screenings (card_slug, screening_json) VALUES ('CARD-C', ?1)",
            rusqlite::params![serde_json::to_string(&json!({"context_md": "retry goal", "acceptance": [{"id":"ac1","intent":"x","done_criteria":"y","kind":"behavior"}]})).unwrap()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO contracts (slug, card_slug, status, failure_kind, rejection_reason_md) VALUES ('CARD-C-CONTRACT-1', 'CARD-C', 'rejected', 'pl_ratify_rejected', 'missing edge case X')",
            [],
        )
        .unwrap();
        let r = tlContractAssemble(&json!({ "card_slug": "CARD-C" }), &db).unwrap();
        assert_eq!(r["contract_slug"], json!("CARD-C-CONTRACT-2"));
        assert_eq!(r["acceptance_kept"], json!(2)); // original ac1 + synthesized ratify-demand
        let prior = crate::db_json::query_rows(&db, "SELECT status, superseded_by FROM contracts WHERE slug = 'CARD-C-CONTRACT-1'", &[]).unwrap();
        assert_eq!(prior[0]["status"], json!("superseded"));
        assert_eq!(prior[0]["superseded_by"], json!("CARD-C-CONTRACT-2"));
        let newc = crate::db_json::query_rows(&db, "SELECT goal_md, acceptance_json FROM contracts WHERE slug = 'CARD-C-CONTRACT-2'", &[]).unwrap();
        assert!(newc[0]["goal_md"].as_str().unwrap().contains("PRIOR RATIFY REJECTION"));
        assert!(newc[0]["acceptance_json"].as_str().unwrap().contains("ratify-demand"));
    }

    #[test]
    fn explicit_screening_id_overrides_latest() {
        let db = make_db();
        db.execute("INSERT INTO cards (slug, title, body_json) VALUES ('CARD-D', 'card d', NULL)", []).unwrap();
        db.execute(
            "INSERT INTO pl_screenings (card_slug, screening_json) VALUES ('CARD-D', ?1)",
            rusqlite::params![serde_json::to_string(&json!({"context_md": "first screening", "acceptance": [{"id":"a1","intent":"i","done_criteria":"d","kind":"behavior"}]})).unwrap()],
        )
        .unwrap();
        let first_id = db.last_insert_rowid();
        db.execute(
            "INSERT INTO pl_screenings (card_slug, screening_json) VALUES ('CARD-D', ?1)",
            rusqlite::params![serde_json::to_string(&json!({"context_md": "second screening", "acceptance": [{"id":"a2","intent":"i2","done_criteria":"d2","kind":"behavior"}]})).unwrap()],
        )
        .unwrap();
        let r = tlContractAssemble(&json!({ "card_slug": "CARD-D", "screening_id": first_id }), &db).unwrap();
        let rows = crate::db_json::query_rows(&db, "SELECT goal_md FROM contracts WHERE slug = ?1", &[r["contract_slug"].clone()]).unwrap();
        assert_eq!(rows[0]["goal_md"], json!("first screening"));
    }
}
