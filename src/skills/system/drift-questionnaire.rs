//! drift-questionnaire.rs — twin of skills/system/drift-questionnaire.mjs (db READ lane,
//! cross-db ATTACH ["brain"] for brain.driver_ticks). NAMES ARE CONTRACT: `driftQuestionnaire`
//! (default). Deterministic 1-3 question A/B/C/D questionnaire for a drift item (diff-<id> or
//! tick-<id>). Cross-module: calls the already-converted `system_card_body_generate::cardBodyGenerate`
//! twin to build a PREVENT card's body for the microtask_path/microtask_spec/sanity_gate Q2.
use regex::Regex;
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
// = JS String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// = JS `${v}`
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

// Number(rawId): JS Number() semantics on the second '-'-split segment (None when the
// drift_id has no second segment, matching JS destructuring of a too-short split array).
fn number_from_opt_str(s: Option<&str>) -> f64 {
    match s {
        None => f64::NAN,
        Some(s) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
    }
}
// Render an id known to be finite (guarded by Number.isFinite before use) as JSON: JS numbers
// carry no int/float distinction, so an integer-valued id renders as an integer (gotcha #2).
fn num_to_json(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 9e15 {
        json!(f as i64)
    } else {
        json!(f)
    }
}
fn num_str(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 9e15 {
        (f as i64).to_string()
    } else {
        f.to_string()
    }
}

pub fn driftQuestionnaire(input: &Value, db: &Connection) -> Result<Value, String> {
    let drift_id_v = input.get("drift_id");
    if !truthy(drift_id_v) {
        return Err("drift-questionnaire: drift_id is required".to_string());
    }
    let drift_id = drift_id_v.unwrap();
    let drift_id_str = str_qq(Some(drift_id)); // String(drift_id)
    let parts: Vec<&str> = drift_id_str.split('-').collect();
    let kind = parts.first().copied().unwrap_or("");
    let raw_id = parts.get(1).copied();
    let num_id = number_from_opt_str(raw_id);

    if !num_id.is_finite() {
        return Ok(json!({
            "drift_id": drift_id.clone(),
            "error": "malformed drift_id (expected \"tick-N\" or \"diff-N\")",
        }));
    }

    if kind == "diff" {
        return diff_questionnaire(db, num_id, drift_id);
    }
    if kind == "tick" {
        return tick_questionnaire(db, num_id, drift_id);
    }
    Ok(json!({ "drift_id": drift_id.clone(), "error": format!("unknown drift kind '{kind}'") }))
}

fn diff_questionnaire(db: &Connection, id: f64, drift_id: &Value) -> Result<Value, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT card_slug, file_path, proposed_by FROM pending_diffs WHERE id = ?",
        &[num_to_json(id)],
    )?;
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Ok(json!({ "drift_id": drift_id.clone(), "error": "pending_diff not found" })),
    };

    let card_slug = row.get("card_slug").cloned().unwrap_or(Value::Null);
    let file_path = row.get("file_path").cloned().unwrap_or(Value::Null);
    let proposed_by = row.get("proposed_by").cloned().unwrap_or(Value::Null);
    let id_json = num_to_json(id);

    let choice_a = json!({
        "label": "A — Approve and apply",
        "dispatch_action": "diff.apply",
        "dispatch_input": { "diff_id": id_json },
        "recommended": true,
    });
    let choice_b = json!({
        "label": "B — Reject (DEV must revise)",
        "dispatch_action": "diff.reject",
        "dispatch_input": { "diff_id": id_json, "reason": "operator-rejected" },
    });
    let payload_json = serde_json::to_string(&json!({ "diff_id": id_json })).unwrap();
    let choice_c = json!({
        "label": "C — Defer (review later)",
        "dispatch_action": "system.notifications.add",
        "dispatch_input": {
            "kind": "generic",
            "severity": "info",
            "source": "web-fe",
            "message": format!("Diff #{} deferred", num_str(id)),
            "payload_json": payload_json,
        },
    });
    let choice_d = json!({ "label": "D — Write your own resolution", "free_text": true });

    Ok(json!({
        "drift_id": drift_id.clone(),
        "slug": card_slug,
        "source_summary": format!(
            "Diff #{} on {} proposed by {}",
            num_str(id),
            tmpl(Some(&file_path)),
            tmpl(Some(&proposed_by)),
        ),
        "questions": [{
            "id": "q1",
            "prompt": "How do you want to handle this proposed patch?",
            "choices": [choice_a, choice_b, choice_c, choice_d],
        }],
    }))
}

fn tick_questionnaire(db: &Connection, id: f64, drift_id: &Value) -> Result<Value, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, card_slug, decided_agent, decided_action, decided_reason, outcome, error_message FROM brain.driver_ticks WHERE id = ?",
        &[num_to_json(id)],
    )?;
    let t = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Ok(json!({ "drift_id": drift_id.clone(), "error": "driver_tick not found" })),
    };

    let error_message = t.get("error_message").cloned().unwrap_or(Value::Null);
    let err_msg = str_qq(t.get("error_message")).to_lowercase();
    let decided = str_qq(t.get("decided_action"));
    let slug = t.get("card_slug").cloned().unwrap_or(Value::Null);

    let mut contract_slug = Value::Null;
    let mut contract_status = Value::Null;
    if let Ok(crows) = crate::db_json::query_rows(
        db,
        "SELECT slug, status FROM contracts WHERE card_slug = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1",
        &[slug.clone()],
    ) {
        if let Some(c) = crows.as_array().and_then(|a| a.first()) {
            contract_slug = c.get("slug").cloned().unwrap_or(Value::Null);
            contract_status = c.get("status").cloned().unwrap_or(Value::Null);
        }
    }

    let categories: &[(&str, &str)] = &[
        (r"microtask_path_invalid|microtasks\[\d+\]\.files_touched", "microtask_path"),
        (r"microtasks?\[\d+\]\.acceptance.*non-empty", "microtask_acceptance"),
        (r"microtasks?\[\d+\]\.max_tokens|max_cost_usd|expected_diff_shape", "microtask_spec"),
        (r"sanity gate refused", "sanity_gate"),
        (r"validation failed|schema", "validation"),
        (r"pl_review_count.*hit", "pl_review_cap"),
        (r"timeout|exceeded", "timeout"),
    ];
    let mut category = "unknown";
    for (pat, key) in categories {
        if Regex::new(pat).unwrap().is_match(&err_msg) {
            category = key;
            break;
        }
    }

    let mut questions: Vec<Value> = Vec::new();

    let q1_prompt = question_prompt_for(category, &decided);
    let q1_choices = choices_for(db, category, &slug, &decided, &t, id, &contract_slug, &contract_status)?;
    questions.push(json!({ "id": "q1", "prompt": q1_prompt, "choices": q1_choices }));

    if category == "microtask_path" || category == "microtask_spec" || category == "sanity_gate" {
        let prevent_input = prevent_card_input(db, &slug, category, &decided, &t)?;
        let choice_a = json!({
            "label": "A — Yes, file a PREVENT card with the pattern",
            "dispatch_action": "board-keeper.card.add",
            "dispatch_input": prevent_input,
        });
        let choice_b = json!({ "label": "B — No, just resolve this instance", "dispatch_action": "noop", "dispatch_input": {} });
        let choice_d = json!({ "label": "D — Write your own resolution", "free_text": true });
        questions.push(json!({
            "id": "q2",
            "prompt": "Should we add a guard so this exact failure can't recur silently?",
            "choices": [choice_a, choice_b, choice_d],
        }));
        // Mark Q2's first choice (file PREVENT) as recommended.
        let last = questions.len() - 1;
        if let Some(choices) = questions[last].get_mut("choices").and_then(Value::as_array_mut) {
            if let Some(first) = choices.first_mut().and_then(Value::as_object_mut) {
                first.insert("recommended".into(), json!(true));
            }
        }
    }

    validate_dispatch_actions(db, &mut questions);

    for q in questions.iter_mut() {
        let has_d = q.get("choices").and_then(Value::as_array)
            .map(|cs| cs.iter().any(|c| truthy(c.get("free_text"))))
            .unwrap_or(false);
        if !has_d {
            if let Some(choices) = q.get_mut("choices").and_then(Value::as_array_mut) {
                choices.push(json!({ "label": "D — Write your own resolution", "free_text": true }));
            }
        }
    }

    Ok(json!({
        "drift_id": drift_id.clone(),
        "slug": slug,
        "source_summary": format!(
            "Tick #{} · {} → {} · failed ({})",
            num_str(id),
            tmpl(t.get("decided_agent")),
            tmpl(t.get("decided_action")),
            category,
        ),
        "category": category,
        "last_error": error_message,
        "questions": questions,
    }))
}

// Build the board-keeper.card.add input for a "file a PREVENT card" choice. Cross-module call to
// the already-converted twin `system_card_body_generate::cardBodyGenerate`.
fn prevent_card_input(db: &Connection, slug: &Value, category: &str, decided: &str, t: &Value) -> Result<Value, String> {
    let cat = category.to_uppercase().replace('_', "-");
    let slug_str = tmpl(Some(slug));
    let card_slug = format!("PREVENT-DRIFT-{cat}-{slug_str}");
    let err_snip: String = str_qq(t.get("error_message")).chars().take(200).collect();
    let why = format!("Repeated failure pattern on {slug_str}: {err_snip}");
    let trigger = format!("{decided} fails with {category}");
    let gen_input = json!({ "kind": "prevent", "slug": card_slug, "why": why, "trigger": trigger });
    let generated = crate::system_card_body_generate::cardBodyGenerate(&gen_input, db)?;
    let body = generated.get("body").cloned().unwrap_or(Value::Null);
    Ok(json!({ "slug": card_slug, "title": format!("Prevent recurrence: {category}"), "body": body }))
}

fn question_prompt_for(category: &str, decided: &str) -> String {
    match category {
        "microtask_path" => "TL refused to sign because microtasks reference invalid file paths. What should happen?".to_string(),
        "microtask_spec" => "TL refused to sign because microtask spec fields (max_tokens / max_cost_usd / expected_diff_shape) are invalid. What should happen?".to_string(),
        "microtask_acceptance" => "TL refused to sign because microtask acceptance criteria are missing or empty. What should happen?".to_string(),
        "sanity_gate" => format!("The sanity gate refused {}. What should happen?", if decided.is_empty() { "this action" } else { decided }),
        "pl_review_cap" => "PL has re-reviewed this contract too many times. MAN intervention required. What should happen?".to_string(),
        "validation" => "Substrate validation refused this action. What should happen?".to_string(),
        "timeout" => "The action exceeded its time budget. What should happen?".to_string(),
        _ => format!("{} failed. What should happen?", if decided.is_empty() { "The last tick" } else { decided }),
    }
}

fn restart_choice(slug: &Value, recommended: bool) -> Value {
    let mut m = Map::new();
    m.insert("label".into(), json!("C — Restart the card (clear progress, back to PL)"));
    m.insert("dispatch_action".into(), json!("system.card.driver.restart"));
    m.insert("dispatch_input".into(), json!({ "slug": slug }));
    if recommended {
        m.insert("recommended".into(), json!(true));
    }
    Value::Object(m)
}
fn retry_tick_choice() -> Value {
    json!({ "label": "B — Retry now (force one driver tick)", "dispatch_action": "system.driver.tick", "dispatch_input": {} })
}
fn freetext_choice() -> Value {
    json!({ "label": "D — Write your own resolution", "free_text": true })
}
fn status_display(contract_status: &Value) -> String {
    match nz(Some(contract_status)) {
        Some(v) => tmpl(Some(v)),
        None => "none".to_string(),
    }
}

fn redraft_choice(contract_slug: &Value, can_redraft: bool, contract_status: &Value) -> Value {
    let mut m = Map::new();
    m.insert(
        "label".into(),
        json!(format!(
            "B — Redraft the contract (TL re-emits from scratch{})",
            if can_redraft { "" } else { " — needs status=rejected" }
        )),
    );
    m.insert("dispatch_action".into(), json!("tl.contract.redraft"));
    m.insert("dispatch_input".into(), json!({ "old_slug": contract_slug }));
    m.insert("recommended".into(), json!(can_redraft));
    m.insert("blocked".into(), json!(!can_redraft));
    if !can_redraft {
        m.insert(
            "blocked_reason".into(),
            json!(format!(
                "redraft only applies on a rejected/superseded contract (current status='{}')",
                status_display(contract_status)
            )),
        );
    }
    Value::Object(m)
}

// Append a status hint suffix to the first choice's label (matches JS withHint).
fn with_hint(mut choices: Vec<Value>, hint: &str) -> Vec<Value> {
    if choices.is_empty() {
        return choices;
    }
    if let Some(first) = choices[0].as_object_mut() {
        let is_free_text = first.get("free_text").map(truthy_bool).unwrap_or(false);
        if !is_free_text {
            if let Some(Value::String(label)) = first.get("label").cloned() {
                first.insert("label".into(), json!(format!("{label}{hint}")));
            }
        }
    }
    choices
}
fn truthy_bool(v: &Value) -> bool {
    truthy(Some(v))
}

#[allow(clippy::too_many_arguments)]
fn choices_for(
    _db: &Connection,
    category: &str,
    slug: &Value,
    _decided: &str,
    _t: &Value,
    _tick_id: f64,
    contract_slug: &Value,
    contract_status: &Value,
) -> Result<Vec<Value>, String> {
    let status_str = contract_status.as_str();
    let can_amend = status_str == Some("impl");
    let can_redraft = status_str == Some("rejected") || status_str == Some("superseded");
    let status_tag = if truthy(Some(contract_status)) {
        format!(" · current contract status='{}'", tmpl(Some(contract_status)))
    } else {
        String::new()
    };

    let amend = if truthy(Some(contract_slug)) { Some(amend_choice(contract_slug, can_amend, contract_status)) } else { None };
    let redraft = if truthy(Some(contract_slug)) { Some(redraft_choice(contract_slug, can_redraft, contract_status)) } else { None };
    let restart_recommended = if truthy(Some(contract_slug)) && !can_amend && !can_redraft {
        restart_choice(slug, true)
    } else {
        restart_choice(slug, false)
    };

    let microtask_cats = ["microtask_path", "microtask_spec", "microtask_acceptance", "sanity_gate"];
    if microtask_cats.contains(&category) {
        let mut out: Vec<Value> = Vec::new();
        if let Some(a) = amend {
            out.push(a);
        }
        if let Some(r) = redraft {
            out.push(r);
        }
        out.push(restart_recommended);
        out.push(freetext_choice());
        return Ok(if !status_tag.is_empty() { with_hint(out, &status_tag) } else { out });
    }
    if category == "pl_review_cap" {
        let mut out: Vec<Value> = Vec::new();
        if let Some(a) = amend {
            out.push(a);
        }
        out.push(retry_tick_choice());
        out.push(restart_recommended);
        out.push(freetext_choice());
        return Ok(out);
    }
    if category == "timeout" {
        return Ok(vec![
            json!({ "label": "A — Retry now (force one driver tick)", "dispatch_action": "system.driver.tick", "dispatch_input": {}, "recommended": true }),
            restart_choice(slug, false),
            freetext_choice(),
        ]);
    }
    // Generic fallback.
    let mut out: Vec<Value> = Vec::new();
    if let Some(a) = amend {
        out.push(a);
    }
    out.push(retry_tick_choice());
    out.push(restart_recommended);
    out.push(freetext_choice());
    Ok(out)
}

fn amend_choice(contract_slug: &Value, can_amend: bool, contract_status: &Value) -> Value {
    let mut m = Map::new();
    m.insert(
        "label".into(),
        json!(format!(
            "A — Amend microtasks in place (TL fixes the broken ones{})",
            if can_amend { "" } else { " — needs status=impl" }
        )),
    );
    m.insert("dispatch_action".into(), json!("tl.contract.amend"));
    m.insert("dispatch_input".into(), json!({ "contract_slug": contract_slug }));
    m.insert("recommended".into(), json!(can_amend));
    m.insert("blocked".into(), json!(!can_amend));
    if !can_amend {
        m.insert(
            "blocked_reason".into(),
            json!(format!(
                "amend only applies while the contract is in impl (current status='{}')",
                status_display(contract_status)
            )),
        );
    }
    Value::Object(m)
}

fn validate_dispatch_actions(db: &Connection, questions: &mut [Value]) {
    let mut allowed: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT name FROM actions", &[]) {
        if let Some(arr) = rows.as_array() {
            for r in arr {
                if let Some(Value::String(n)) = r.get("name") {
                    allowed.insert(n.clone());
                }
            }
        }
    }
    for q in questions.iter_mut() {
        let choices = match q.get_mut("choices").and_then(Value::as_array_mut) {
            Some(c) => c,
            None => continue,
        };
        for c in choices.iter_mut() {
            let obj = match c.as_object_mut() {
                Some(o) => o,
                None => continue,
            };
            if truthy(obj.get("free_text")) {
                continue;
            }
            let action = match obj.get("dispatch_action") {
                Some(Value::String(s)) if truthy(Some(&Value::String(s.clone()))) => s.clone(),
                _ => continue,
            };
            if action == "noop" {
                continue;
            }
            if !allowed.contains(&action) {
                if let Some(Value::String(label)) = obj.get("label").cloned() {
                    obj.insert("label".into(), json!(format!("{label} (action unavailable — fallback: restart)")));
                }
                obj.insert("dispatch_action".into(), json!("system.card.driver.restart"));
                let slug = obj.get("dispatch_input").and_then(|d| d.get("slug")).cloned();
                let new_input = match slug {
                    Some(s) if truthy(Some(&s)) => json!({ "slug": s }),
                    _ => obj.get("dispatch_input").cloned().unwrap_or_else(|| json!({})),
                };
                obj.insert("dispatch_input".into(), new_input);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(db: &Connection) {
        db.execute_batch(
            "CREATE TABLE pending_diffs (id INTEGER PRIMARY KEY, card_slug TEXT, file_path TEXT, proposed_by TEXT);
             CREATE TABLE contracts (id INTEGER PRIMARY KEY, slug TEXT, card_slug TEXT, status TEXT, superseded_by INTEGER);
             CREATE TABLE actions (name TEXT PRIMARY KEY);
             ATTACH DATABASE ':memory:' AS brain;
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY, card_slug TEXT, decided_agent TEXT, decided_action TEXT, decided_reason TEXT, outcome TEXT, error_message TEXT);",
        )
        .unwrap();
    }

    #[test]
    fn malformed_and_unknown_kind() {
        let db = Connection::open_in_memory().unwrap();
        schema(&db);
        let r = driftQuestionnaire(&json!({ "drift_id": "bogus" }), &db).unwrap();
        assert_eq!(r["error"], json!("malformed drift_id (expected \"tick-N\" or \"diff-N\")"));
        let r2 = driftQuestionnaire(&json!({ "drift_id": "weird-1" }), &db).unwrap();
        assert_eq!(r2["error"], json!("unknown drift kind 'weird'"));
    }

    #[test]
    fn diff_not_found_and_found() {
        let db = Connection::open_in_memory().unwrap();
        schema(&db);
        let miss = driftQuestionnaire(&json!({ "drift_id": "diff-99" }), &db).unwrap();
        assert_eq!(miss["error"], json!("pending_diff not found"));

        db.execute(
            "INSERT INTO pending_diffs (id, card_slug, file_path, proposed_by) VALUES (1,'FEAT-X','src/a.mjs','dev')",
            [],
        )
        .unwrap();
        let r = driftQuestionnaire(&json!({ "drift_id": "diff-1" }), &db).unwrap();
        assert_eq!(r["slug"], json!("FEAT-X"));
        assert_eq!(r["source_summary"], json!("Diff #1 on src/a.mjs proposed by dev"));
        let choices = r["questions"][0]["choices"].as_array().unwrap();
        assert_eq!(choices.len(), 4);
        assert_eq!(choices[0]["dispatch_input"], json!({ "diff_id": 1 }));
        assert!(choices[2]["dispatch_input"]["payload_json"].as_str().unwrap().contains("\"diff_id\":1"));
    }

    #[test]
    fn tick_not_found() {
        let db = Connection::open_in_memory().unwrap();
        schema(&db);
        let r = driftQuestionnaire(&json!({ "drift_id": "tick-5" }), &db).unwrap();
        assert_eq!(r["error"], json!("driver_tick not found"));
    }

    #[test]
    fn tick_microtask_path_with_amend_and_prevent_card() {
        let db = Connection::open_in_memory().unwrap();
        schema(&db);
        db.execute(
            "INSERT INTO brain.driver_ticks (id, card_slug, decided_agent, decided_action, decided_reason, outcome, error_message)
             VALUES (1,'FEAT-Y','tl','tl.contract.sign','x','fail','microtask_path_invalid at index 0')",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO contracts (id, slug, card_slug, status, superseded_by) VALUES (1,'CONTRACT-Y','FEAT-Y','impl',NULL)", []).unwrap();
        db.execute("INSERT INTO actions (name) VALUES ('tl.contract.amend'),('tl.contract.redraft'),('system.card.driver.restart'),('board-keeper.card.add'),('noop')", []).unwrap();

        let r = driftQuestionnaire(&json!({ "drift_id": "tick-1" }), &db).unwrap();
        assert_eq!(r["category"], json!("microtask_path"));
        assert_eq!(r["slug"], json!("FEAT-Y"));
        let qs = r["questions"].as_array().unwrap();
        assert_eq!(qs.len(), 2);
        let q1_choices = qs[0]["choices"].as_array().unwrap();
        assert_eq!(q1_choices[0]["dispatch_action"], json!("tl.contract.amend"));
        assert_eq!(q1_choices[0]["recommended"], json!(true));
        assert_eq!(q1_choices[0]["blocked"], json!(false));
        assert!(q1_choices[0].get("blocked_reason").is_none());
        assert_eq!(q1_choices[1]["dispatch_action"], json!("tl.contract.redraft"));
        assert_eq!(q1_choices[1]["recommended"], json!(false));
        assert_eq!(q1_choices[1]["blocked"], json!(true));
        assert!(q1_choices[1]["blocked_reason"].as_str().unwrap().contains("current status='impl'"));
        assert!(q1_choices[0]["label"].as_str().unwrap().ends_with("current contract status='impl'"));

        let q2_choices = qs[1]["choices"].as_array().unwrap();
        assert_eq!(q2_choices.len(), 3);
        assert_eq!(q2_choices[0]["recommended"], json!(true));
        let body = &q2_choices[0]["dispatch_input"]["body"];
        assert_eq!(body["kind"], json!("prevent"));
        assert_eq!(q2_choices[0]["dispatch_input"]["slug"], json!("PREVENT-DRIFT-MICROTASK-PATH-FEAT-Y"));
    }

    #[test]
    fn tick_timeout_category() {
        let db = Connection::open_in_memory().unwrap();
        schema(&db);
        db.execute(
            "INSERT INTO brain.driver_ticks (id, card_slug, decided_agent, decided_action, decided_reason, outcome, error_message)
             VALUES (2,'FEAT-Z','dev','dev.tick','x','fail','operation timeout exceeded')",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO actions (name) VALUES ('system.driver.tick'),('system.card.driver.restart')", []).unwrap();
        let r = driftQuestionnaire(&json!({ "drift_id": "tick-2" }), &db).unwrap();
        assert_eq!(r["category"], json!("timeout"));
        let choices = r["questions"][0]["choices"].as_array().unwrap();
        assert_eq!(choices[0]["recommended"], json!(true));
        assert_eq!(choices[0]["dispatch_action"], json!("system.driver.tick"));
    }

    #[test]
    fn tick_unknown_category_fallback_action_unavailable() {
        let db = Connection::open_in_memory().unwrap();
        schema(&db);
        db.execute(
            "INSERT INTO brain.driver_ticks (id, card_slug, decided_agent, decided_action, decided_reason, outcome, error_message)
             VALUES (3,'FEAT-W','dev','dev.tick','x','fail','something odd happened')",
            [],
        )
        .unwrap();
        // no actions rows at all, no contract row: both RETRY_TICK and RESTART's own action
        // are "not allowed" -> both get the unavailable-fallback label swap (restart's own
        // fallback IS itself, a real JS quirk verified against the live skill).
        let r = driftQuestionnaire(&json!({ "drift_id": "tick-3" }), &db).unwrap();
        assert_eq!(r["category"], json!("unknown"));
        let choices = r["questions"][0]["choices"].as_array().unwrap();
        assert_eq!(choices.len(), 3);
        // RETRY_TICK (system.driver.tick) not allowed -> swapped to restart; its ORIGINAL
        // dispatch_input {} has no 'slug' key so it stays {} (not rewritten to {slug}).
        assert_eq!(choices[0]["dispatch_action"], json!("system.card.driver.restart"));
        assert!(choices[0]["label"].as_str().unwrap().contains("action unavailable"));
        assert_eq!(choices[0]["dispatch_input"], json!({}));
        // RESTART_RECOMMENDED's own action (system.card.driver.restart) is also "not allowed"
        // here -> self-swap: label gets the suffix too, dispatch_input keeps its slug key.
        assert!(choices[1]["label"].as_str().unwrap().contains("action unavailable"));
        assert_eq!(choices[1]["dispatch_input"], json!({ "slug": "FEAT-W" }));
    }
}
