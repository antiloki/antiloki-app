//! man-card-review.rs — twin of skills/agents/man/man-card-review.mjs (db lane, cross-db
//! brain.*/memory.* reads+writes). NAMES ARE CONTRACT: `manCardReview` (default). Read-only audit
//! of a finished card's journey: pulls sacred_timeline events, runtime_events (scoped to the
//! card's pl_sessions via user_goal_id), qa_runs for its contract, brain.ai_calls cost/duration,
//! computes a DETERMINISTIC verdict (pass/concern/fail) + findings — NO AI for the verdict itself.
//! Always records a card_phase_events row (phase='man_reviewed') via the already-converted
//! `entities_card_phase_event_record::cardPhaseEventRecord` twin so the state machine sees the
//! audit step happened.
//!
//! PARITY-DEFERRED: the Slice D "Phase 2" lessons-distill step (FEAT-REJECTION-RECOVERY-AND-
//! MAN-AUDIT) — reached only when a contract exists, `distill_lessons` is truthy, and the
//! deterministic verdict isn't 'fail' — ends in `crate::ai_ai_call::aiCall`, the deferred
//! always-`Err` network-AI stub (see ai-call.rs's module doc). Everything up to and including that
//! call is ported faithfully: the `man_reports` INSERT (best-effort, matches JS's try/catch —
//! table-missing on a legacy DB is swallowed exactly like the JS `catch (e) {}`), the
//! `ensure_lessons_prompt` bootstrap (reusing the already-converted `ai_prompt_find`/`ai_prompt_add`
//! twins, mirroring book-run.rs's `ensure_faculty`), and the goal/metrics prompt-input assembly.
//! The response-parsing (`out.find(...)`, tags flattening) and the post-call `man_reports` UPDATE +
//! `entities_memory_add::memoryAdd` write are ported byte-for-byte from the JS but are unreachable
//! by this file's own fixture — like book-run.rs's post-wave staging — because they run only after
//! a successful `aiCall`, which the deferred stub never produces; correctness there is by
//! inspection + reuse of the already-converted, independently-tested `memoryAdd` twin.
//!
//! `context.provider_fn` (a JS function reference threaded opaquely into aiCall's input) has no
//! Rust equivalent — passed as `null`, same convention as book-run.rs/man-interpret-diagnosis.rs.
//! `context.db` missing → JS throws `err.contextMissing('man-card-review', 'db')`: unreachable here
//! (`db: &Connection` is a required, non-nullable reference, same convention as every sibling
//! db-lane twin).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
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
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn first(rows: &Value) -> Option<Value> {
    rows.as_array().and_then(|a| a.first()).cloned()
}

// `db.prepare(sql).all(...params)` wrapped in try/catch → [] on any error (missing table etc).
fn safe_all(db: &Connection, sql: &str, params: &[Value]) -> Value {
    crate::db_json::query_rows(db, sql, params).unwrap_or_else(|_| json!([]))
}
// `db.prepare(sql).get(...params)` wrapped in try/catch → null on any error.
fn safe_get(db: &Connection, sql: &str, params: &[Value]) -> Value {
    first(&safe_all(db, sql, params)).unwrap_or(Value::Null)
}

const LESSONS_PROMPT_NAME: &str = "man.review.lessons.v1";
const LESSONS_PROMPT_TEMPLATE: &str = "You are MAN — the auditor. A contract just passed QA. Distill ONE concrete lesson the substrate can use on FUTURE contracts.

═══ CONTRACT GOAL ═══
{{goal_md}}

═══ WHAT HAPPENED ═══
{{what_happened}}

═══ METRICS ═══
{{metrics_block}}

Rules:
  - Lesson is ONE short paragraph (≤ 100 words). Imperative voice (\"When X, do Y because Z\").
  - Focus on what's TRANSFERABLE — a heuristic the next contract on similar work should follow.
  - If verdict was 'fail' (rare for this prompt — we only call on pass/concern), say so honestly and explain why no positive lesson applies.

Output JSON of this exact shape:
{
  \"title\":     \"<≤ 80-char headline for memory.memories>\",
  \"lesson_md\": \"<the lesson paragraph>\",
  \"tags\":      [\"<area>\", \"<technique>\"]
}";

fn ensure_lessons_prompt(db: &Connection) -> Result<(), String> {
    if crate::ai_prompt_find::promptFind(&json!({ "name": LESSONS_PROMPT_NAME }), db).is_ok() {
        return Ok(());
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": LESSONS_PROMPT_NAME,
            "template_md": LESSONS_PROMPT_TEMPLATE,
            "default_tier": "low",
            "default_temperature": "cold",
            "max_output_tokens": 600,
            "output_fields": [
                { "field_name": "title",     "field_type": "string" },
                { "field_name": "lesson_md", "field_type": "string" },
                { "field_name": "tags",      "field_type": "array_of_string" },
            ],
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!({ "name": LESSONS_PROMPT_NAME }), db)?;
    Ok(())
}

fn find_contract_for(db: &Connection, card_slug: &str) -> Value {
    safe_get(
        db,
        "SELECT slug, status, tier FROM contracts \
         WHERE card_slug = ? AND COALESCE(superseded_by, '') = '' \
         ORDER BY id DESC LIMIT 1",
        &[json!(card_slug)],
    )
}
fn find_sessions_for(db: &Connection, card_slug: &str) -> Value {
    safe_all(
        db,
        "SELECT id, status FROM pl_sessions WHERE approved_card_slug = ? ORDER BY id ASC",
        &[json!(card_slug)],
    )
}
fn timeline_events_for(db: &Connection, card_slug: &str) -> Value {
    safe_all(
        db,
        "SELECT id, ts, operation, actor FROM sacred_timeline \
         WHERE entity_type = 'card' AND entity_id = ? ORDER BY id ASC",
        &[json!(card_slug)],
    )
}
fn runtime_events_for_sessions(db: &Connection, session_ids: &[i64]) -> Value {
    if session_ids.is_empty() {
        return json!([]);
    }
    let placeholders = session_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT id, ts, action, outcome, error_code FROM runtime_events \
         WHERE user_goal_id IN ({placeholders}) ORDER BY id ASC"
    );
    let params: Vec<Value> = session_ids.iter().map(|id| json!(id)).collect();
    safe_all(db, &sql, &params)
}
fn qa_runs_for_contract(db: &Connection, contract_slug: Option<&str>) -> Value {
    match contract_slug {
        None => json!([]),
        Some(slug) => safe_all(
            db,
            "SELECT id, ts, all_passed, acceptance_pass, acceptance_fail \
             FROM qa_runs WHERE contract_slug = ? ORDER BY id ASC",
            &[json!(slug)],
        ),
    }
}
fn ai_calls_for_sessions(db: &Connection, session_ids: &[i64]) -> Value {
    if session_ids.is_empty() {
        return json!([]);
    }
    let placeholders = session_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT a.id, a.cost_usd_actual, a.duration_ms, p.name AS prompt_name \
         FROM brain.ai_calls a \
         LEFT JOIN brain.prompt_templates p ON a.prompt_template_id = p.id \
         JOIN runtime_events r ON r.id = a.parent_event_id \
         WHERE r.user_goal_id IN ({placeholders})"
    );
    let params: Vec<Value> = session_ids.iter().map(|id| json!(id)).collect();
    safe_all(db, &sql, &params)
}

fn as_rows(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

struct Facts {
    card_slug: String,
    contract: Value,
    timeline_events: Vec<Value>,
    runtime_events: Vec<Value>,
    qa_runs: Vec<Value>,
    ai_calls: Vec<Value>,
}

fn total_cost(ai_calls: &[Value]) -> f64 {
    // `.sum()` on an empty f64 iterator yields -0.0 in Rust, but the JS twin's
    // `reduce((s, c) => s + c, 0)` yields +0.0 — fold with an explicit init to match.
    ai_calls
        .iter()
        .map(|c| c.get("cost_usd_actual").and_then(Value::as_f64).unwrap_or(0.0))
        .fold(0.0, |a, b| a + b)
}
fn total_ms(ai_calls: &[Value]) -> f64 {
    ai_calls
        .iter()
        .map(|c| c.get("duration_ms").and_then(Value::as_f64).unwrap_or(0.0))
        .fold(0.0, |a, b| a + b)
}

// `/VALIDATION_LOGIC|PERMISSION_DENIED|INTERNAL_ERROR/.test(error_code ?? '')`
fn is_critical_error_code(code: &str) -> bool {
    code.contains("VALIDATION_LOGIC") || code.contains("PERMISSION_DENIED") || code.contains("INTERNAL_ERROR")
}

fn derive_verdict(facts: &Facts) -> (String, Vec<Value>) {
    let mut findings: Vec<Value> = Vec::new();

    if facts.timeline_events.is_empty() {
        findings.push(json!({
            "severity": "fail", "kind": "no_audit_trail",
            "snippet": "sacred_timeline has zero events for this card",
            "ref": facts.card_slug,
        }));
    }

    let critical_errors: Vec<&Value> = facts
        .runtime_events
        .iter()
        .filter(|r| {
            r.get("outcome").and_then(Value::as_str) == Some("error")
                && is_critical_error_code(&js_string(r.get("error_code").unwrap_or(&Value::Null)))
        })
        .collect();
    if !critical_errors.is_empty() {
        let codes: Vec<String> = critical_errors
            .iter()
            .take(3)
            .map(|e| js_string(e.get("error_code").unwrap_or(&Value::Null)))
            .collect();
        let first_id = js_string(critical_errors[0].get("id").unwrap_or(&Value::Null));
        findings.push(json!({
            "severity": "concern", "kind": "runtime_errors",
            "snippet": format!("{} runtime event(s) failed: {}", critical_errors.len(), codes.join(", ")),
            "ref": format!("rt-{first_id}"),
        }));
    }

    if !facts.contract.is_null() {
        let last_qa = facts.qa_runs.last();
        match last_qa {
            None => {
                findings.push(json!({
                    "severity": "concern", "kind": "no_qa_run",
                    "snippet": "contract reached done but no qa_runs row exists",
                    "ref": facts.contract.get("slug").cloned().unwrap_or(Value::Null),
                }));
            }
            Some(qa) => {
                let all_passed = qa.get("all_passed");
                let is_1 = all_passed.and_then(Value::as_i64) == Some(1);
                if !is_1 {
                    let ap = js_string(qa.get("acceptance_pass").unwrap_or(&Value::Null));
                    let af = js_string(qa.get("acceptance_fail").unwrap_or(&Value::Null));
                    let ap_val = js_string(all_passed.unwrap_or(&Value::Null));
                    let qid = js_string(qa.get("id").unwrap_or(&Value::Null));
                    findings.push(json!({
                        "severity": "fail", "kind": "qa_did_not_pass",
                        "snippet": format!("last qa_run.all_passed={ap_val}; acceptance pass={ap} fail={af}"),
                        "ref": format!("qa-{qid}"),
                    }));
                }
            }
        }
    }

    let cost = total_cost(&facts.ai_calls);
    if cost > 1.0 {
        findings.push(json!({
            "severity": "concern", "kind": "high_cost",
            "snippet": format!("total AI cost ${cost:.4} exceeds $1.00"),
            "ref": facts.card_slug,
        }));
    }

    let has_fail = findings.iter().any(|f| f["severity"] == json!("fail"));
    let has_concern = findings.iter().any(|f| f["severity"] == json!("concern"));
    let verdict = if has_fail { "fail" } else if has_concern { "concern" } else { "pass" };
    (verdict.to_string(), findings)
}

fn build_summary(card_slug: &str, facts: &Facts, verdict: &str, findings: &[Value]) -> String {
    let mut lines: Vec<String> = vec![format!("# Card review: {card_slug}"), String::new()];
    lines.push(format!("**Verdict:** `{verdict}`"));
    lines.push(format!("**Timeline events:** {}", facts.timeline_events.len()));
    let rt_errors = facts.runtime_events.iter().filter(|r| r.get("outcome").and_then(Value::as_str) == Some("error")).count();
    lines.push(format!("**Runtime events:** {} (errors: {rt_errors})", facts.runtime_events.len()));
    let qa_suffix = if let Some(last) = facts.qa_runs.last() {
        format!(" (last: all_passed={})", js_string(last.get("all_passed").unwrap_or(&Value::Null)))
    } else {
        String::new()
    };
    lines.push(format!("**QA runs:** {}{qa_suffix}", facts.qa_runs.len()));
    let cost = total_cost(&facts.ai_calls);
    let ms = total_ms(&facts.ai_calls);
    lines.push(format!("**AI calls:** {} · ${cost:.4} · {}s", facts.ai_calls.len(), (ms / 1000.0).round() as i64));
    if !findings.is_empty() {
        lines.push(String::new());
        lines.push("## Findings".to_string());
        for f in findings {
            lines.push(format!(
                "- **{}** [{}] {} (`{}`)",
                js_string(f.get("severity").unwrap_or(&Value::Null)),
                js_string(f.get("kind").unwrap_or(&Value::Null)),
                js_string(f.get("snippet").unwrap_or(&Value::Null)),
                js_string(f.get("ref").unwrap_or(&Value::Null)),
            ));
        }
    } else {
        lines.push(String::new());
        lines.push("_No concerns. Card progressed cleanly through the pipeline._".to_string());
    }
    lines.join("\n")
}

fn to_report_verdict(v: &str) -> &'static str {
    match v {
        "pass" => "approved",
        "concern" => "flagged",
        _ => "blocked",
    }
}

#[allow(non_snake_case)]
pub fn manCardReview(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, actor = 'man:auto', provider_fn = null } = context ?? {};` — actor and
    // provider_fn come from CONTEXT, not input. Under the db-lane harness context is always
    // `{ db }` (see entities_memory_add's doc), so `context.actor` is always undefined and the
    // default always fires: hardcoded here, same convention as book-run.rs's provider_fn.
    let actor = "man:auto".to_string();
    let provider_fn = Value::Null;
    let distill_lessons_v = input.get("distill_lessons").cloned().unwrap_or(json!(true));
    let card_slug_v = input.get("card_slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&card_slug_v)) {
        return Err("man-card-review: card_slug is required".to_string());
    }
    let card_slug = js_string(&card_slug_v);

    let card = safe_get(db, "SELECT slug, status FROM cards WHERE slug = ?", &[json!(card_slug.clone())]);
    if card.is_null() {
        return Err(format!("man-card-review: card '{card_slug}' not found"));
    }

    let contract = find_contract_for(db, &card_slug);
    let sessions = find_sessions_for(db, &card_slug);
    let session_ids: Vec<i64> = as_rows(&sessions).iter().filter_map(|s| s.get("id").and_then(Value::as_i64)).collect();
    let timeline_events = as_rows(&timeline_events_for(db, &card_slug));
    let runtime_events = as_rows(&runtime_events_for_sessions(db, &session_ids));
    let contract_slug_s = contract.get("slug").and_then(Value::as_str).map(|s| s.to_string());
    let qa_runs = as_rows(&qa_runs_for_contract(db, contract_slug_s.as_deref()));
    let ai_calls = as_rows(&ai_calls_for_sessions(db, &session_ids));

    let facts = Facts {
        card_slug: card_slug.clone(),
        contract: contract.clone(),
        timeline_events,
        runtime_events,
        qa_runs,
        ai_calls,
    };
    let (verdict, findings) = derive_verdict(&facts);
    let summary_md = build_summary(&card_slug, &facts, &verdict, &findings);

    // Always record the phase event so the state machine sees this step is done.
    let phase_event_id = match crate::entities_card_phase_event_record::cardPhaseEventRecord(
        &json!({
            "card_slug": card_slug,
            "phase": "man_reviewed",
            "actor": actor,
            "evidence": {
                "verdict": verdict,
                "findings_count": findings.len(),
                "findings": findings,
                "source": "man-card-review",
            },
        }),
        db,
    ) {
        Ok(ev) => ev.get("id").cloned().unwrap_or(Value::Null),
        Err(_) => Value::Null, // don't let evidence-logging block the verdict
    };

    let ai_call_count = facts.ai_calls.len();
    let total_ai_cost_usd = total_cost(&facts.ai_calls);
    let total_ai_duration_ms = total_ms(&facts.ai_calls);
    let metrics = json!({
        "timeline_event_count": facts.timeline_events.len(),
        "runtime_event_count": facts.runtime_events.len(),
        "qa_run_count": facts.qa_runs.len(),
        "ai_call_count": ai_call_count,
        "total_ai_cost_usd": total_ai_cost_usd,
        "total_ai_duration_ms": total_ai_duration_ms,
    });

    let mut man_report_id: Value = Value::Null;
    let mut lessons_md: Value = Value::Null;
    let mut memory_id: Value = Value::Null;

    if !contract.is_null() {
        let mut criteria: Map<String, Value> = Map::new();
        for f in &findings {
            let kind = f.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
            criteria.insert(kind, json!({ "severity": f.get("severity"), "snippet": f.get("snippet") }));
        }
        let criteria_json = serde_json::to_string(&Value::Object(criteria)).map_err(|e| e.to_string())?;
        let metrics_json = serde_json::to_string(&metrics).map_err(|e| e.to_string())?;
        let contract_slug = contract.get("slug").cloned().unwrap_or(Value::Null);

        if let Ok((_c, id)) = crate::db_json::run_write(
            db,
            "INSERT INTO man_reports (contract_slug, phase, verdict, metrics_json, criteria_json, what_happened_md) \
             VALUES (?, 'qa_passed', ?, ?, ?, ?)",
            &[contract_slug.clone(), json!(to_report_verdict(&verdict)), json!(metrics_json), json!(criteria_json), json!(summary_md.clone())],
        ) {
            man_report_id = json!(id);
        }
        // table missing on legacy DB — swallowed exactly like the JS `catch (e) {}`.

        if truthy(Some(&distill_lessons_v)) && verdict != "fail" && !man_report_id.is_null() {
            let attempt = (|| -> Result<(Value, Value), String> {
                ensure_lessons_prompt(db)?;
                let goal_row = safe_get(db, "SELECT goal_md FROM contracts WHERE slug = ?", &[contract_slug.clone()]);
                let goal_md: String = goal_row.get("goal_md").and_then(Value::as_str).unwrap_or("").chars().take(4000).collect();
                let what_happened: String = summary_md.chars().take(3000).collect();
                let metrics_block = [
                    format!("verdict: {verdict}"),
                    format!(
                        "runtime events: {} (errors: {})",
                        facts.runtime_events.len(),
                        facts.runtime_events.iter().filter(|r| r.get("outcome").and_then(Value::as_str) == Some("error")).count(),
                    ),
                    format!("QA runs: {}", facts.qa_runs.len()),
                    format!(
                        "AI: {} calls, ${total_ai_cost_usd:.4}, {}s",
                        ai_call_count,
                        (total_ai_duration_ms / 1000.0).round() as i64
                    ),
                ]
                .join("\n");

                let ai_input = json!({
                    "prompt_name": LESSONS_PROMPT_NAME,
                    "input_kv": [
                        { "key": "goal_md",       "value": goal_md },
                        { "key": "what_happened", "value": what_happened },
                        { "key": "metrics_block", "value": metrics_block },
                    ],
                    "provider_fn": provider_fn,
                });
                let ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;

                let out = ai_result.get("output_kv").cloned().unwrap_or(json!([]));
                let out_rows = as_rows(&out);
                let title = out_rows
                    .iter()
                    .find(|o| o.get("field_name").and_then(Value::as_str) == Some("title"))
                    .and_then(|o| o.get("value_text").cloned())
                    .filter(|v| !v.is_null())
                    .unwrap_or_else(|| json!(format!("Lesson from {card_slug}")));
                let lesson_md_v = out_rows
                    .iter()
                    .find(|o| o.get("field_name").and_then(Value::as_str) == Some("lesson_md"))
                    .and_then(|o| o.get("value_text").cloned())
                    .filter(|v| !v.is_null())
                    .unwrap_or(Value::Null);

                let tags_parents: Vec<&Value> = out_rows
                    .iter()
                    .filter(|o| o.get("field_name").and_then(Value::as_str) == Some("tags") && o.get("value_text").map_or(true, Value::is_null))
                    .collect();
                let tags_arr: Vec<Value> = if let Some(parent) = tags_parents.first() {
                    let parent_id = parent.get("id").cloned();
                    out_rows
                        .iter()
                        .filter(|o| o.get("parent_output_id").cloned() == parent_id && parent_id.is_some())
                        .filter_map(|o| o.get("value_text").cloned())
                        .filter(|v| truthy(Some(v)))
                        .collect()
                } else {
                    vec![]
                };

                Ok((title, if lesson_md_v.is_null() { Value::Null } else { json!({ "lesson_md": lesson_md_v, "tags": tags_arr }) }))
            })();

            match attempt {
                Ok((title, lesson_obj)) if !lesson_obj.is_null() => {
                    let lesson_text = lesson_obj.get("lesson_md").cloned().unwrap_or(Value::Null);
                    lessons_md = lesson_text.clone();
                    let _ = crate::db_json::run_write(
                        db,
                        "UPDATE man_reports SET lessons_md = ? WHERE id = ?",
                        &[lesson_text.clone(), man_report_id.clone()],
                    );
                    let tier = contract.get("tier").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("standard").to_string();
                    let mut tags = vec![json!("man-audit"), json!("phase2"), json!(tier)];
                    if let Some(arr) = lesson_obj.get("tags").and_then(Value::as_array) {
                        tags.extend(arr.iter().cloned());
                    }
                    let title_s: String = js_string(&title).chars().take(200).collect();
                    if let Ok(mem) = crate::entities_memory_add::memoryAdd(
                        &json!({
                            "type": "lesson",
                            "title": title_s,
                            "situation": format!("Contract '{}' completed (verdict={verdict}); MAN Phase 2 audit.", js_string(&contract_slug)),
                            "guidance": lesson_text,
                            "tags": tags,
                            "created_by": actor,
                            "source_card": card_slug,
                            "confidence": 0.7,
                        }),
                        db,
                    ) {
                        memory_id = mem.get("id").cloned().unwrap_or(Value::Null);
                    }
                    // memory writer not available on legacy DB — swallowed exactly like JS.
                }
                Ok(_) => {
                    // lesson_md came back empty/null — JS only writes when `if (lessons_md)`.
                }
                Err(_) => {
                    // AI failure or schema issue — report row still landed, just no lesson.
                    lessons_md = Value::Null;
                }
            }
        }
    }

    Ok(json!({
        "card_slug": card_slug,
        "verdict": verdict,
        "findings": findings,
        "summary_md": summary_md,
        "phase_event_id": phase_event_id,
        "metrics": metrics,
        "man_report_id": man_report_id,
        "lessons_md": lessons_md,
        "memory_id": memory_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; ATTACH ':memory:' AS memory;").unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT);
             CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, card_slug TEXT, status TEXT, tier TEXT, superseded_by TEXT, goal_md TEXT);
             CREATE TABLE pl_sessions (id INTEGER PRIMARY KEY AUTOINCREMENT, status TEXT, approved_card_slug TEXT);
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, entity_type TEXT, entity_id TEXT, operation TEXT, actor TEXT);
             CREATE TABLE runtime_events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, user_goal_id INTEGER, action TEXT, outcome TEXT, error_code TEXT);
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, contract_slug TEXT, all_passed INTEGER, acceptance_pass INTEGER, acceptance_fail INTEGER);
             CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, phase TEXT, actor TEXT, evidence_json TEXT);
             CREATE TABLE man_reports (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, phase TEXT, verdict TEXT, metrics_json TEXT, criteria_json TEXT, what_happened_md TEXT, lessons_md TEXT);
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, cost_usd_actual REAL, duration_ms INTEGER, prompt_template_id INTEGER, parent_event_id INTEGER);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE memory.memories (
               id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL, situation TEXT NOT NULL,
               guidance TEXT NOT NULL, naive_response TEXT, rationale TEXT, tags_json TEXT NOT NULL DEFAULT '[]',
               source_card TEXT, source_event INTEGER, source_session TEXT, confidence INTEGER NOT NULL DEFAULT 0,
               validated INTEGER NOT NULL DEFAULT 0, superseded_by INTEGER, agent_slug TEXT,
               created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), created_by TEXT NOT NULL,
               CHECK (type IN ('lesson','preference','decision','reference','correction','narrative')),
               CHECK (confidence >= 0 AND confidence <= 1), CHECK (validated IN (0,1))
             );",
        ).unwrap();
        c
    }

    #[test]
    fn requires_card_slug() {
        let c = setup();
        let e = manCardReview(&json!({}), &c).unwrap_err();
        assert_eq!(e, "man-card-review: card_slug is required");
    }

    #[test]
    fn card_not_found() {
        let c = setup();
        let e = manCardReview(&json!({ "card_slug": "NOPE" }), &c).unwrap_err();
        assert_eq!(e, "man-card-review: card 'NOPE' not found");
    }

    #[test]
    fn no_contract_no_timeline_yields_fail_verdict_and_records_phase_event() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, status) VALUES ('C-1', 'done')", []).unwrap();
        let r = manCardReview(&json!({ "card_slug": "C-1" }), &c).unwrap();
        assert_eq!(r["verdict"], json!("fail"));
        assert_eq!(r["findings"][0]["kind"], json!("no_audit_trail"));
        assert_eq!(r["man_report_id"], Value::Null); // no contract → no man_reports row
        assert!(r["phase_event_id"].is_number());
        let n: i64 = c.query_row("SELECT COUNT(*) FROM card_phase_events WHERE card_slug='C-1' AND phase='man_reviewed'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn clean_timeline_with_passing_qa_yields_pass_verdict() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, status) VALUES ('C-2', 'done')", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (entity_type, entity_id, operation, actor) VALUES ('card','C-2','status.change','pl')", []).unwrap();
        c.execute("INSERT INTO contracts (slug, card_slug, tier) VALUES ('CT-2','C-2','standard')", []).unwrap();
        c.execute("INSERT INTO qa_runs (contract_slug, all_passed, acceptance_pass, acceptance_fail) VALUES ('CT-2', 1, 5, 0)", []).unwrap();
        let r = manCardReview(&json!({ "card_slug": "C-2", "distill_lessons": false }), &c).unwrap();
        assert_eq!(r["verdict"], json!("pass"));
        assert_eq!(r["findings"], json!([]));
        assert!(r["man_report_id"].is_number());
        let (verdict, phase): (String, String) = c.query_row("SELECT verdict, phase FROM man_reports WHERE id = ?", [r["man_report_id"].as_i64().unwrap()], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!((verdict.as_str(), phase.as_str()), ("approved", "qa_passed"));
        // distill_lessons: false → the AI leg never runs
        assert_eq!(r["lessons_md"], Value::Null);
        assert_eq!(r["memory_id"], Value::Null);
    }

    #[test]
    fn qa_not_passed_yields_fail_finding_and_blocked_report_verdict() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, status) VALUES ('C-3', 'done')", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (entity_type, entity_id, operation, actor) VALUES ('card','C-3','status.change','pl')", []).unwrap();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('CT-3','C-3')", []).unwrap();
        c.execute("INSERT INTO qa_runs (contract_slug, all_passed, acceptance_pass, acceptance_fail) VALUES ('CT-3', 0, 2, 3)", []).unwrap();
        let r = manCardReview(&json!({ "card_slug": "C-3", "distill_lessons": false }), &c).unwrap();
        assert_eq!(r["verdict"], json!("fail"));
        assert_eq!(r["findings"][0]["kind"], json!("qa_did_not_pass"));
        let verdict: String = c.query_row("SELECT verdict FROM man_reports WHERE id = ?", [r["man_report_id"].as_i64().unwrap()], |row| row.get(0)).unwrap();
        assert_eq!(verdict, "blocked");
    }

    #[test]
    fn contract_with_no_qa_run_is_a_concern() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, status) VALUES ('C-4', 'done')", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (entity_type, entity_id, operation, actor) VALUES ('card','C-4','status.change','pl')", []).unwrap();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('CT-4','C-4')", []).unwrap();
        let r = manCardReview(&json!({ "card_slug": "C-4", "distill_lessons": false }), &c).unwrap();
        assert_eq!(r["verdict"], json!("concern"));
        assert_eq!(r["findings"][0]["kind"], json!("no_qa_run"));
        let verdict: String = c.query_row("SELECT verdict FROM man_reports WHERE id = ?", [r["man_report_id"].as_i64().unwrap()], |row| row.get(0)).unwrap();
        assert_eq!(verdict, "flagged");
    }

    #[test]
    fn critical_runtime_errors_and_high_cost_are_concerns() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, status) VALUES ('C-5', 'done')", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (entity_type, entity_id, operation, actor) VALUES ('card','C-5','status.change','pl')", []).unwrap();
        c.execute("INSERT INTO pl_sessions (status, approved_card_slug) VALUES ('done','C-5')", []).unwrap();
        let sid = c.last_insert_rowid();
        c.execute("INSERT INTO runtime_events (user_goal_id, action, outcome, error_code) VALUES (?, 'x', 'error', 'PERMISSION_DENIED')", [sid]).unwrap();
        c.execute("INSERT INTO runtime_events (user_goal_id, action, outcome, error_code) VALUES (?, 'y', 'error', 'OTHER')", [sid]).unwrap();
        c.execute("INSERT INTO brain.ai_calls (cost_usd_actual, duration_ms, parent_event_id) VALUES (1.5, 2000, 1)", []).unwrap();
        let r = manCardReview(&json!({ "card_slug": "C-5" }), &c).unwrap();
        assert_eq!(r["verdict"], json!("concern"));
        let kinds: Vec<String> = r["findings"].as_array().unwrap().iter().map(|f| f["kind"].as_str().unwrap().to_string()).collect();
        assert!(kinds.contains(&"runtime_errors".to_string()));
        assert!(kinds.contains(&"high_cost".to_string()));
        assert_eq!(r["metrics"]["ai_call_count"], json!(1));
    }

    #[test]
    fn pass_verdict_with_distill_lessons_reaches_parity_deferred_aicall_and_report_still_lands() {
        // proves the deterministic gate (man_reports INSERT, ensure_lessons_prompt bootstrap) ran
        // cleanly before the deferred aiCall stub short-circuits the lesson write.
        let c = setup();
        c.execute("INSERT INTO cards (slug, status) VALUES ('C-6', 'done')", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (entity_type, entity_id, operation, actor) VALUES ('card','C-6','status.change','pl')", []).unwrap();
        c.execute("INSERT INTO contracts (slug, card_slug, tier, goal_md) VALUES ('CT-6','C-6','critical','ship the feature')", []).unwrap();
        c.execute("INSERT INTO qa_runs (contract_slug, all_passed) VALUES ('CT-6', 1)", []).unwrap();
        let r = manCardReview(&json!({ "card_slug": "C-6" }), &c).unwrap();
        assert_eq!(r["verdict"], json!("pass"));
        assert!(r["man_report_id"].is_number()); // report landed
        assert_eq!(r["lessons_md"], Value::Null); // deferred aiCall → no lesson
        assert_eq!(r["memory_id"], Value::Null);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'man.review.lessons.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1); // ensure_lessons_prompt bootstrapped the template before the deferred call
        let lessons: Option<String> = c.query_row("SELECT lessons_md FROM man_reports WHERE id = ?", [r["man_report_id"].as_i64().unwrap()], |row| row.get(0)).unwrap();
        assert_eq!(lessons, None);
    }

    #[test]
    fn summary_md_matches_js_shape() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, status) VALUES ('C-7', 'done')", []).unwrap();
        let r = manCardReview(&json!({ "card_slug": "C-7" }), &c).unwrap();
        let md = r["summary_md"].as_str().unwrap();
        assert!(md.starts_with("# Card review: C-7\n\n**Verdict:** `fail`\n**Timeline events:** 0\n**Runtime events:** 0 (errors: 0)\n**QA runs:** 0\n**AI calls:** 0 · $0.0000 · 0s"));
        assert!(md.contains("## Findings"));
        assert!(md.contains("- **fail** [no_audit_trail] sacred_timeline has zero events for this card (`C-7`)"));
    }

    #[test]
    fn default_actor_is_man_auto() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, status) VALUES ('C-8', 'done')", []).unwrap();
        manCardReview(&json!({ "card_slug": "C-8" }), &c).unwrap();
        let actor: String = c.query_row("SELECT actor FROM card_phase_events WHERE card_slug='C-8'", [], |row| row.get(0)).unwrap();
        assert_eq!(actor, "man:auto");
    }
}
