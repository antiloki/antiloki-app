//! pl-ratify-contract.rs — twin of skills/blueprint/pl-ratify-contract.mjs (db lane). NAMES ARE
//! CONTRACT: default `plRatifyContract`. PL Ratify (pipeline v2 PLAN step 4): does the CONTRACT
//! satisfy the CARD? Compares the contract's acceptance (S-tasks, each linked to a card feature
//! via `card_acceptance_id`) against the card's acceptance (the features). Two checks:
//!   1. coverage (DETERMINISTIC): every card feature has >=1 contract S-task linked to it.
//!   2. satisfies (AI, mid): do the linked S-tasks faithfully fulfil each feature?
//! Verdict TRUE -> contract -> pl_ratified. FALSE -> rejected + rejection_reason_md. Coverage is
//! a HARD gate (a false coverage forces a false verdict regardless of the AI).
//!
//! PARITY-DEFERRED (partial, not whole-function): the AI verdict is dispatched through
//! `crate::ai_ai_call::aiCall`, the deferred network-AI stub (see ai-call.rs's module doc) — it
//! always returns `Err`. UNLIKE book-run.rs/pl-research.rs (where the aiCall failure propagates
//! via `?` and short-circuits the rest of the function), the JS here wraps the call in a
//! `try { } catch { /* AI down -> fall back to the coverage verdict */ }` that SWALLOWS the
//! error — so this twin's `aiCall` call is likewise non-propagating (matched, error ignored) and
//! the ENTIRE function — guard, contract/card lookups, JSON parsing, coverage computation, the
//! (always-failing) AI attempt, verdict/reason resolution, and the contracts UPDATE — is real,
//! reachable, deterministic surface, fixture-testable end to end. Frozen clock 2026-01-01 in
//! place of `datetime('now')` per the crossing playbook.
// QUALITY: full unit ported — deterministic surface reaches every branch; the one non-deterministic
// call site (aiCall) is dispatched-and-swallowed exactly like the JS try/catch.
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{json, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
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
} // = JS String(v ?? '')
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
} // = JS `${v}`

const PROMPT: &str = "pl.ratify.contract.v1";
const SYSTEM_MD: &str = "You are the PRODUCT LEAD ratifying a contract. You compare the contract acceptance against the card acceptance and decide TRUE/FALSE: does the contract faithfully satisfy the card? You output only strict JSON.";
const TEMPLATE: &str = "Ratify: does the CONTRACT satisfy the CARD? Compare the two acceptance lists.

=== CARD ACCEPTANCE (the features the card wants) ===
{{card_acceptance}}

=== CONTRACT ACCEPTANCE (the S-tasks; each links to a feature via card_acceptance_id) ===
{{contract_acceptance}}

Decide: do the contract's S-tasks, taken together, FAITHFULLY satisfy every card feature —
nothing important missing, nothing out of scope?

Output JSON (nothing else — no prose, no fences) into verdict_json, EXACTLY:
{\"satisfies\": true, \"reason\": \"<1-2 sentences; if false, EXACTLY what is missing or wrong, for the rewind>\"}";

// try promptFind; on miss, promptAdd then re-check (mirrors JS ensurePrompt: skip the re-add if
// the existing template already matches verbatim).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    if let Ok(existing) = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db) {
        if existing.get("template_md").and_then(Value::as_str) == Some(TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM_MD,
            "default_tier": "mid",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 2000,
            "output_fields": [{ "field_name": "verdict_json", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

// `JSON.parse(s ?? '[]')` + `Array.isArray` guard, `[]` on any failure (bad JSON, non-array).
fn parse_json_arr(v: Option<&Value>) -> Vec<Value> {
    let default = matches!(v, None | Some(Value::Null));
    let s = if default { "[]".to_string() } else { js_string(v.unwrap()) };
    match serde_json::from_str::<Value>(&s) {
        Ok(Value::Array(a)) => a,
        _ => vec![],
    }
}

fn obj_field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_object().and_then(|o| o.get(key))
}
// `String(a?.card_acceptance_id)` / `String(f?.id)` — optional chaining off a possibly non-object
// array element yields `undefined` -> `String(undefined)` === "undefined".
fn js_string_opt(v: Option<&Value>) -> String {
    match v {
        None => "undefined".to_string(),
        Some(x) => js_string(x),
    }
}

// `JSON.stringify(arr, null, 1).slice(0, n)` — pretty-print with a 1-space indent (serde's
// preserve_order keeps the same key order as the JS object literals), then a UTF-16-safe slice
// (playbook rule 5: `.slice` = take `n` chars; fixtures stay ASCII/BMP).
fn json_stringify_pretty1(v: &Value, take: usize) -> String {
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    v.serialize(&mut ser).expect("serialize");
    let s = String::from_utf8(buf).expect("utf8");
    s.chars().take(take).collect()
}

pub fn plRatifyContract(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent).
    let provider_fn = Value::Null;
    let contract_slug_v = input.get("contract_slug");
    if !truthy(contract_slug_v) {
        return Err("pl-ratify-contract: contract_slug is required".to_string());
    }
    let contract_slug = str_qq(contract_slug_v);

    let contract_rows = crate::db_json::query_rows(
        db,
        "SELECT slug, card_slug, status, acceptance_json FROM contracts WHERE slug = ?",
        &[json!(contract_slug)],
    )?;
    let Some(contract) = contract_rows.as_array().and_then(|a| a.first()).cloned() else {
        return Err(format!("pl-ratify-contract: contract '{contract_slug}' not found"));
    };
    let card_slug = obj_field(&contract, "card_slug").cloned().unwrap_or(Value::Null);

    let card_rows = crate::db_json::query_rows(db, "SELECT slug, acceptance_json FROM cards WHERE slug = ?", &[card_slug])?;
    let card = card_rows.as_array().and_then(|a| a.first()).cloned();

    let card_acc = parse_json_arr(card.as_ref().and_then(|c| obj_field(c, "acceptance_json")));
    let contract_acc = parse_json_arr(obj_field(&contract, "acceptance_json"));

    // 1. coverage (deterministic): every card feature has >=1 contract S-task linked.
    let linked: std::collections::HashSet<String> = contract_acc.iter().map(|a| js_string_opt(obj_field(a, "card_acceptance_id"))).collect();
    let uncovered: Vec<Value> = card_acc.iter().filter(|f| !linked.contains(&js_string_opt(obj_field(f, "id")))).cloned().collect();
    let coverage_passed = uncovered.is_empty();
    let coverage_uncovered: Vec<Value> = uncovered.iter().map(|f| obj_field(f, "id").cloned().unwrap_or(Value::Null)).collect();

    // 2. satisfies (AI) — coverage is the hard floor.
    ensure_prompt(db)?;
    let mut ai_satisfies = true;
    // `coverage.passed ? '...' : \`uncovered card features: ${uncovered.map(f=>\`${f?.id} (${f?.intent})\`).join('; ')}\``
    let mut reason = if coverage_passed {
        "all card features covered by the contract".to_string()
    } else {
        format!(
            "uncovered card features: {}",
            uncovered.iter().map(|f| format!("{} ({})", tmpl(obj_field(f, "id")), tmpl(obj_field(f, "intent")))).collect::<Vec<_>>().join("; ")
        )
    };

    if !card_acc.is_empty() {
        let ai_input = json!({
            "prompt_name": PROMPT,
            "input_kv": [
                { "key": "card_acceptance", "value": json_stringify_pretty1(&Value::Array(card_acc.clone()), 6000) },
                { "key": "contract_acceptance", "value": json_stringify_pretty1(&Value::Array(contract_acc.clone()), 8000) },
            ],
            "tier": "mid",
            "temperature": "cold",
            "phase_flow": "single_only",
            "provider_fn": provider_fn,
        });
        // JS: `try { ... } catch { /* AI down -> fall back to the coverage verdict */ }` — the
        // deferred aiCall stub always errs, so this branch is a no-op every time (matches the JS
        // fallback path exactly, not merely approximates it).
        if let Ok(res) = crate::ai_ai_call::aiCall(&ai_input, db) {
            let v = crate::blueprint_screening_gate::parseLoose(&crate::blueprint_screening_gate::pickField(&res, &json!("verdict_json")));
            if let Some(sat) = v.get("satisfies").and_then(Value::as_bool) {
                ai_satisfies = sat;
                if let Some(r) = v.get("reason").filter(|x| !x.is_null()) {
                    reason = js_string(r);
                }
            }
        }
    }

    let verdict = coverage_passed && ai_satisfies;
    if !coverage_passed && ai_satisfies {
        reason = format!("uncovered card features: {}", coverage_uncovered.iter().map(js_string).collect::<Vec<_>>().join(", "));
    }

    if verdict {
        crate::db_json::run_write(
            db,
            &format!("UPDATE contracts SET status = 'pl_ratified', updated_at = {} WHERE slug = ? AND status = 'proposed'", crate::clock::now_sql()),
            &[json!(contract_slug)],
        )?;
    } else {
        crate::db_json::run_write(
            db,
            &format!("UPDATE contracts SET status = 'rejected', failure_kind = COALESCE(failure_kind, 'pl_ratify_rejected'), rejection_reason_md = ?, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
            &[json!(reason), json!(contract_slug)],
        )?;
    }

    let coverage = json!({ "passed": coverage_passed, "uncovered": coverage_uncovered });
    Ok(json!({
        "contract_slug": contract_slug,
        "ratified": verdict,
        "verdict": verdict,
        "reason": reason,
        "coverage": coverage,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT, status TEXT, acceptance_json TEXT, failure_kind TEXT, rejection_reason_md TEXT, updated_at TEXT);
             CREATE TABLE cards (slug TEXT PRIMARY KEY, acceptance_json TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn contract_slug_is_required() {
        let c = setup();
        let e = plRatifyContract(&json!({}), &c).unwrap_err();
        assert_eq!(e, "pl-ratify-contract: contract_slug is required");
        let e2 = plRatifyContract(&json!({ "contract_slug": "" }), &c).unwrap_err();
        assert_eq!(e2, "pl-ratify-contract: contract_slug is required");
    }

    #[test]
    fn contract_not_found_errors() {
        let c = setup();
        let e = plRatifyContract(&json!({ "contract_slug": "C-404" }), &c).unwrap_err();
        assert_eq!(e, "pl-ratify-contract: contract 'C-404' not found");
    }

    #[test]
    fn full_coverage_and_empty_card_acceptance_ratifies_via_coverage_only() {
        // card has NO acceptance -> `cardAcc.length > 0` is false, the AI branch is skipped
        // entirely, aiSatisfies stays true, coverage.passed stays true (vacuously: no features to
        // cover) -> verdict true, contract flips proposed -> pl_ratified.
        let c = setup();
        c.execute("INSERT INTO cards (slug, acceptance_json) VALUES ('CARD-1', '[]')", []).unwrap();
        c.execute("INSERT INTO contracts (slug, card_slug, status, acceptance_json) VALUES ('C-1', 'CARD-1', 'proposed', '[]')", []).unwrap();
        let r = plRatifyContract(&json!({ "contract_slug": "C-1" }), &c).unwrap();
        assert_eq!(r["ratified"], json!(true));
        assert_eq!(r["verdict"], json!(true));
        assert_eq!(r["reason"], json!("all card features covered by the contract"));
        assert_eq!(r["coverage"], json!({ "passed": true, "uncovered": [] }));
        let status: String = c.query_row("SELECT status FROM contracts WHERE slug = 'C-1'", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "pl_ratified");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'pl.ratify.contract.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn uncovered_feature_forces_false_verdict_and_rejects_the_contract() {
        // card has 1 feature (id 'f1'); contract's S-task links to a DIFFERENT id -> uncovered ->
        // coverage.passed=false -> the AI (would-be) satisfies=true from the deferred-stub fallback
        // is overridden back to the coverage-derived rejection reason (the JS's second `if` branch).
        let c = setup();
        c.execute(
            "INSERT INTO cards (slug, acceptance_json) VALUES ('CARD-2', '[{\"id\":\"f1\",\"intent\":\"do the thing\"}]')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, status, acceptance_json) VALUES ('C-2', 'CARD-2', 'proposed', '[{\"card_acceptance_id\":\"other\"}]')",
            [],
        )
        .unwrap();
        let r = plRatifyContract(&json!({ "contract_slug": "C-2" }), &c).unwrap();
        assert_eq!(r["ratified"], json!(false));
        assert_eq!(r["coverage"], json!({ "passed": false, "uncovered": ["f1"] }));
        assert_eq!(r["reason"], json!("uncovered card features: f1"));
        let (status, rejection): (String, String) = c
            .query_row("SELECT status, rejection_reason_md FROM contracts WHERE slug = 'C-2'", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        assert_eq!(status, "rejected");
        assert_eq!(rejection, "uncovered card features: f1");
    }

    #[test]
    fn full_coverage_with_nonempty_card_acceptance_still_ratifies_via_deferred_ai_fallback() {
        // full coverage AND cardAcc.length > 0 -> the AI branch DOES run (dispatch-and-swallow the
        // deferred stub), leaving aiSatisfies=true untouched -> verdict stays true.
        let c = setup();
        c.execute(
            "INSERT INTO cards (slug, acceptance_json) VALUES ('CARD-3', '[{\"id\":\"f1\",\"intent\":\"do it\"}]')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, status, acceptance_json) VALUES ('C-3', 'CARD-3', 'proposed', '[{\"card_acceptance_id\":\"f1\"}]')",
            [],
        )
        .unwrap();
        let r = plRatifyContract(&json!({ "contract_slug": "C-3" }), &c).unwrap();
        assert_eq!(r["ratified"], json!(true));
        assert_eq!(r["coverage"], json!({ "passed": true, "uncovered": [] }));
        let status: String = c.query_row("SELECT status FROM contracts WHERE slug = 'C-3'", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "pl_ratified");
    }

    #[test]
    fn non_proposed_status_is_not_flipped_by_a_ratified_verdict() {
        // WHERE ... AND status = 'proposed' guard: an already-terminal contract's status row is
        // untouched even though the function still computes/returns a true verdict.
        let c = setup();
        c.execute("INSERT INTO cards (slug, acceptance_json) VALUES ('CARD-4', '[]')", []).unwrap();
        c.execute("INSERT INTO contracts (slug, card_slug, status, acceptance_json) VALUES ('C-4', 'CARD-4', 'pl_ratified', '[]')", []).unwrap();
        let r = plRatifyContract(&json!({ "contract_slug": "C-4" }), &c).unwrap();
        assert_eq!(r["ratified"], json!(true));
        let status: String = c.query_row("SELECT status FROM contracts WHERE slug = 'C-4'", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "pl_ratified"); // unchanged (guard didn't match 'proposed')
    }

    #[test]
    fn malformed_acceptance_json_falls_back_to_empty_arrays() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, acceptance_json) VALUES ('CARD-5', 'not json')", []).unwrap();
        c.execute("INSERT INTO contracts (slug, card_slug, status, acceptance_json) VALUES ('C-5', 'CARD-5', 'proposed', NULL)", []).unwrap();
        let r = plRatifyContract(&json!({ "contract_slug": "C-5" }), &c).unwrap();
        assert_eq!(r["coverage"], json!({ "passed": true, "uncovered": [] }));
        assert_eq!(r["ratified"], json!(true));
    }

    #[test]
    fn parse_json_arr_matches_js_json_parse_default_and_guard() {
        assert_eq!(parse_json_arr(None), Vec::<Value>::new());
        assert_eq!(parse_json_arr(Some(&Value::Null)), Vec::<Value>::new());
        assert_eq!(parse_json_arr(Some(&json!("[1,2,3]"))), vec![json!(1), json!(2), json!(3)]);
        assert_eq!(parse_json_arr(Some(&json!("{}"))), Vec::<Value>::new()); // valid JSON, not an array
        assert_eq!(parse_json_arr(Some(&json!("not json"))), Vec::<Value>::new());
    }

    #[test]
    fn json_stringify_pretty1_uses_one_space_indent() {
        // verified against real JS: JSON.stringify([{"id":"f1"}], null, 1)
        let out = json_stringify_pretty1(&json!([{ "id": "f1" }]), 6000);
        assert_eq!(out, "[\n {\n  \"id\": \"f1\"\n }\n]");
    }
}
