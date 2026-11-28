//! cockpit-plan-judge.rs — twin of skills/cockpit/cockpit-plan-judge.mjs (db, cross-db
//! judgments.judgment_cases, git subprocess). NAMES ARE CONTRACT: default `cockpitPlanJudge`.
//! The JUDGMENT pane: judges ONE plan item's acceptance criteria against EVIDENCE gathered from
//! the session's panes (worktree diffs via a real `git` subprocess, coordination blackboard
//! notes). `mode` starting with "tribunal" delegates whole to the already-converted
//! `cockpit_cockpit_plan_tribunal::cockpitPlanTribunal` twin (mirrors the JS early-return).
//!
//! PARITY-DEFERRED: the single mid-tier AI call that rules every criterion ends in
//! `crate::ai_ai_call::aiCall` (the deferred network-AI stub — see ai-call.rs's module doc).
//! Everything that surrounds it is ported faithfully and IS the deterministic surface this
//! file's tests exercise: the tribunal-mode delegation, schema ensure, session_id/item guards,
//! plan-item lookup (or free-form criteria), `gatherEvidence` (a real `git` subprocess wrapper —
//! mirrors cockpit-pane-diff.rs's / cockpit-git-graph.rs's `git_run` pattern, exercised on its
//! own no-worktree-rows fallback path), `ensurePrompt`'s find-or-add against the already-converted
//! `ai_prompt_find`/`ai_prompt_add` twins, and the aiCall input-kv assembly (item title/intent,
//! the approved-blueprint shape summary, numbered criteria, evidence). The response-parsing
//! helpers (verdicts JSON extraction/normalization, overall computation) and the post-wave
//! plan-writeback / judgments.judgment_cases best-effort insert are ported byte-for-byte from the
//! JS — like book-run.rs's post-wave staging — but are unreachable by this file's own fixture
//! since they run only after a successful `aiCall`, which the deferred stub never produces;
//! correctness there is by inspection + the same helper shapes as the reachable code.
use rusqlite::Connection;
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
}
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
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
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

const PROMPT: &str = "cockpit-plan-judge";
const SYSTEM: &str = "You are the JUDGE in a multi-agent coding cockpit. For EACH acceptance criterion, rule strictly from the EVIDENCE (diffs, files, agent notes): pass only when the evidence SHOWS it done; fail when the evidence shows it not done or done wrong; needs_human when the evidence cannot decide (UI look, product taste, external systems). Output ONLY valid JSON: {\"verdicts\":[{\"criterion\":\"…\",\"verdict\":\"pass|fail|needs_human\",\"reasoning\":\"one anchored sentence\"}]}. Same order as given. No prose outside the JSON.";
const TEMPLATE: &str = "ITEM: {{item_title}}\nINTENT: {{intent}}\n\nACCEPTANCE CRITERIA:\n{{criteria}}\n\nEVIDENCE (from every pane in the session):\n{{evidence}}\n\nRule each criterion.";

// mirrors JS ensurePrompt's try/catch find, skip-if-same-template, else add.
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(|v| v.as_str()) == Some(TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT, "template_md": TEMPLATE, "system_md": SYSTEM,
            "default_tier": "mid", "default_temperature": "freezed", "phase_flow": "single_only", "max_output_tokens": 2000,
            "output_fields": [{ "field_name": "verdicts_json", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

/// `const git = (args, cwd) => { try { return execFileSync(...); } catch { return ''; } }` —
/// execFileSync returns stdout on a zero exit; a nonzero exit (or a missing `git` binary) throws,
/// caught into `''`. Mirrors cockpit-pane-diff.rs's / cockpit-git-graph.rs's `git_run` pattern.
fn git(args: &[&str], cwd: &str) -> String {
    match crate::proc::cmd("git").args(args).current_dir(cwd).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => String::new(),
    }
}

fn gather_evidence(db: &Connection, session_id: i64) -> Result<String, String> {
    let panes_rows = crate::db_json::query_rows(
        db,
        "SELECT id, idx, subagent_name, pane_kind, worktree_path, branch FROM cockpit_panes WHERE session_id = ?",
        &[json!(session_id)],
    )?;
    let panes: Vec<Value> = panes_rows.as_array().cloned().unwrap_or_default();
    let mut parts: Vec<String> = Vec::new();
    for p in &panes {
        let wt = str_qq(p.get("worktree_path"));
        if wt.is_empty() { continue; }
        let mut stat = git(&["diff", "--stat", "HEAD"], &wt);
        if stat.is_empty() { stat = git(&["diff", "--stat"], &wt); }
        let diff_full = git(&["diff", "HEAD"], &wt);
        let diff: String = diff_full.chars().take(6000).collect();
        let log = git(&["log", "--oneline", "-5"], &wt);
        if !stat.is_empty() || !diff.is_empty() || !log.is_empty() {
            let idx = js_number(p.get("idx"));
            let idx_disp = if idx.is_finite() { ((idx as i64) + 1).to_string() } else { "NaN".to_string() };
            let subagent = js_string(p.get("subagent_name").unwrap_or(&Value::Null));
            let branch = nz(p.get("branch")).map(js_string).unwrap_or_else(|| "worktree".to_string());
            parts.push(format!(
                "── PANE #{idx_disp} {subagent} ({branch}) ──\nrecent commits:\n{log}\ndirty stat:\n{stat}\ndiff (capped):\n{diff}"
            ));
        }
    }
    let coord_rows = crate::db_json::query_rows(
        db,
        "SELECT kind, body_md FROM cockpit_blackboard WHERE session_id = ? AND pane_id IS NULL AND kind NOT IN ('context') ORDER BY id DESC LIMIT 30",
        &[json!(session_id)],
    )?;
    let coord_lines: Vec<String> = coord_rows.as_array().cloned().unwrap_or_default().iter().map(|c| {
        let kind = js_string(c.get("kind").unwrap_or(&Value::Null));
        let body: String = js_string(c.get("body_md").unwrap_or(&Value::Null)).chars().take(300).collect();
        format!("[{kind}] {body}")
    }).collect();
    let coord = coord_lines.join("\n");
    if !coord.is_empty() {
        parts.push(format!("── COORDINATION (what the agents wrote each other) ──\n{coord}"));
    }
    let joined = parts.join("\n\n");
    let capped: String = joined.chars().take(24000).collect();
    Ok(if capped.is_empty() { "(no evidence yet — no worktrees, no coordination)".to_string() } else { capped })
}

pub fn cockpitPlanJudge(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `if (String(input?.mode ?? '').startsWith('tribunal')) return cockpitPlanTribunal(...)`
    let mode = str_qq(nz(input.get("mode")));
    if mode.starts_with("tribunal") {
        return crate::cockpit_cockpit_plan_tribunal::cockpitPlanTribunal(input, db);
    }
    let _ = crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));
    let session_id_num = js_number(input.get("session_id"));
    if session_id_num == 0.0 || session_id_num.is_nan() {
        return Err("cockpit-plan-judge: session_id is required".to_string());
    }
    let session_id = session_id_num as i64;

    // the item: from the plan pane's plan row, or free-form criteria
    let plan_pane_id_num = match nz(input.get("plan_pane_id")) {
        Some(v) => js_number(Some(v)),
        None => 0.0,
    };
    let plan_pane_id = if plan_pane_id_num.is_finite() { plan_pane_id_num as i64 } else { 0 };
    let has_item_id = truthy(input.get("item_id"));

    let item: Value;
    let mut plan: Option<Value> = None;
    let mut plan_row: Option<Value> = None;

    if plan_pane_id != 0 && !plan_pane_id_num.is_nan() && has_item_id {
        let rows = crate::db_json::query_rows(
            db,
            "SELECT id, body_md FROM cockpit_blackboard WHERE session_id = ? AND pane_id = ? AND kind = 'plan' ORDER BY id DESC LIMIT 1",
            &[json!(session_id), json!(plan_pane_id)],
        )?;
        let mut found_item: Option<Value> = None;
        if let Some(row) = first(&rows) {
            let body_md = js_string(row.get("body_md").unwrap_or(&Value::Null));
            if let Ok(parsed) = serde_json::from_str::<Value>(&body_md) {
                if let Some(phases) = parsed.get("phases").and_then(|v| v.as_array()) {
                    'outer: for ph in phases {
                        if let Some(items) = ph.get("items").and_then(|v| v.as_array()) {
                            for it in items {
                                if it.get("id") == input.get("item_id") {
                                    found_item = Some(it.clone());
                                    break 'outer;
                                }
                            }
                        }
                    }
                }
                plan = Some(parsed);
            }
            plan_row = Some(row);
        }
        match found_item {
            Some(it) => item = it,
            None => {
                return Ok(json!({ "ok": false, "error": format!("item {} not found in the plan", tmpl(input.get("item_id"))) }));
            }
        }
    } else {
        let crit_raw = input.get("criteria").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let crit: Vec<String> = crit_raw.iter().map(js_string).filter(|s| !s.is_empty()).collect();
        if crit.is_empty() {
            return Err("cockpit-plan-judge: item_id or criteria is required".to_string());
        }
        let title_raw = nz(input.get("title")).cloned().unwrap_or(json!("free-form judgment"));
        let title: String = js_string(&title_raw).chars().take(140).collect();
        item = json!({ "id": "freeform", "title": title, "intent": "", "acceptance": crit });
    }

    let evidence = gather_evidence(db, session_id)?;
    ensure_prompt(db)?;

    let item_title = js_string(item.get("title").unwrap_or(&Value::Null));
    let intent_part1 = if truthy(item.get("intent")) { js_string(item.get("intent").unwrap()) } else { "(none)".to_string() };
    let blueprint_approved = truthy(item.get("blueprint")) && truthy(item.get("blueprint_approved"));
    let intent_part2 = if blueprint_approved {
        let files = item.get("blueprint").and_then(|b| b.get("files")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let lines: Vec<String> = files.iter().map(|f| {
            let path = js_string(f.get("path").unwrap_or(&Value::Null));
            let action = js_string(f.get("action").unwrap_or(&Value::Null));
            let symbols = f.get("symbols").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let syms: Vec<String> = symbols.iter().map(|sy| {
                let kind = js_string(sy.get("kind").unwrap_or(&Value::Null));
                let name = js_string(sy.get("name").unwrap_or(&Value::Null));
                let sig = if truthy(sy.get("signature")) { format!(" {}", js_string(sy.get("signature").unwrap())) } else { String::new() };
                format!("{kind} {name}{sig}")
            }).collect();
            format!("{path} ({action}): {}", syms.join("; "))
        }).collect();
        format!("APPROVED BLUEPRINT (the diff should match this shape):\n{}", lines.join("\n"))
    } else {
        String::new()
    };
    let intent: String = [intent_part1, intent_part2].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n").chars().take(4000).collect();

    let acceptance = item.get("acceptance").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let criteria: String = acceptance.iter().enumerate().map(|(i, c)| format!("{}. {}", i + 1, js_string(c))).collect::<Vec<_>>().join("\n");

    let input_kv = json!([
        { "key": "item_title", "value": item_title },
        { "key": "intent", "value": intent },
        { "key": "criteria", "value": criteria },
        { "key": "evidence", "value": evidence },
    ]);
    let ai_input = json!({ "prompt_name": PROMPT, "tier": "mid", "input_kv": input_kv, "provider_fn": Value::Null });
    // ── PARITY-DEFERRED past this point (see module doc): aiCall always Err()s. Everything below
    // is ported byte-for-byte but unreachable by this file's own fixture — same convention as
    // book-run.rs's post-wave staging.
    let res = crate::ai_ai_call::aiCall(&ai_input, db)?;

    let output_kv = res.get("output_kv").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let found = output_kv.iter().find(|o| {
        o.get("field_name").and_then(|v| v.as_str()) == Some("verdicts_json")
            && o.get("parent_output_id").map_or(true, |v| v.is_null())
    });
    let text_val = nz(found.and_then(|o| o.get("value_text"))).cloned().unwrap_or(json!(""));
    let text = js_string(&text_val);
    let ai_call_id = res.get("ai_call_id").cloned().unwrap_or(Value::Null);

    let trimmed = text.trim().to_string();
    let raw = if trimmed.starts_with('{') {
        trimmed.clone()
    } else {
        let re = regex::Regex::new(r"(?s)\{.*\}").unwrap();
        re.find(&trimmed).map(|m| m.as_str().to_string()).unwrap_or_default()
    };

    let parse_result: Result<Vec<Value>, String> = match serde_json::from_str::<Value>(&raw) {
        Ok(v) => {
            let arr = v.get("verdicts").and_then(|x| x.as_array()).cloned().unwrap_or_default();
            let out: Vec<Value> = arr.iter().enumerate().map(|(i, x)| {
                let criterion_val = nz(x.get("criterion")).cloned()
                    .or_else(|| acceptance.get(i).cloned())
                    .unwrap_or(json!(""));
                let criterion = js_string(&criterion_val);
                let verdict_str = x.get("verdict").and_then(|v| v.as_str()).unwrap_or("");
                let verdict = if ["pass", "fail", "needs_human"].contains(&verdict_str) { verdict_str.to_string() } else { "needs_human".to_string() };
                let reasoning_val = nz(x.get("reasoning")).cloned().unwrap_or(json!(""));
                let reasoning: String = js_string(&reasoning_val).chars().take(400).collect();
                json!({ "criterion": criterion, "verdict": verdict, "reasoning": reasoning })
            }).collect();
            if out.is_empty() { Err("no verdicts".to_string()) } else { Ok(out) }
        }
        Err(e) => Err(e.to_string()),
    };

    let verdicts = match parse_result {
        Err(msg) => {
            return Ok(json!({ "ok": false, "error": format!("verdicts did not parse: {msg}"), "ai_call_id": ai_call_id }));
        }
        Ok(v) => v,
    };

    let overall = if verdicts.iter().all(|v| v.get("verdict") == Some(&json!("pass"))) {
        "pass"
    } else if verdicts.iter().any(|v| v.get("verdict") == Some(&json!("fail"))) {
        "fail"
    } else {
        "needs_human"
    };

    // write the verdict back onto the plan item (the plan is the spine)
    if let (Some(plan_val), Some(prow)) = (plan.as_ref(), plan_row.as_ref()) {
        let mut plan_mut = plan_val.clone();
        let item_id = item.get("id").cloned().unwrap_or(Value::Null);
        if let Some(phases) = plan_mut.get_mut("phases").and_then(|v| v.as_array_mut()) {
            for ph in phases.iter_mut() {
                if let Some(items) = ph.get_mut("items").and_then(|v| v.as_array_mut()) {
                    for it in items.iter_mut() {
                        if it.get("id") == Some(&item_id) {
                            if let Some(o) = it.as_object_mut() {
                                o.insert("status".into(), json!(overall));
                                o.insert("verdicts".into(), json!(verdicts));
                            }
                        }
                    }
                }
            }
        }
        let body: String = serde_json::to_string(&plan_mut).map_err(|e| e.to_string())?.chars().take(19000).collect();
        let prow_id = prow.get("id").cloned().unwrap_or(Value::Null);
        crate::db_json::run_write(db, "UPDATE cockpit_blackboard SET body_md = ? WHERE id = ?", &[json!(body), prow_id])?;
    }

    // the COURT (judgments.judgment_cases) — best-effort, never blocks the verdict
    let item_title_200: String = js_string(item.get("title").unwrap_or(&Value::Null)).chars().take(200).collect();
    let item_id_disp = tmpl(item.get("id"));
    let _ = crate::db_json::run_write(
        db,
        &format!("INSERT INTO judgments.judgment_cases (card_slug, scope_kind, scope_ref, goal_line, n_items, verdict, created_at, updated_at) VALUES (?,?,?,?,?,?, {}, {})", crate::clock::now_sql(), crate::clock::now_sql()),
        &[
            json!(format!("VIBE-SESSION-{session_id}")),
            json!("vibe_item"),
            json!(format!("session:{session_id}:item:{item_id_disp}")),
            json!(item_title_200),
            json!(verdicts.len() as i64),
            json!(overall),
        ],
    );

    Ok(json!({
        "ok": true,
        "item_id": item.get("id").cloned().unwrap_or(Value::Null),
        "verdicts": verdicts,
        "overall": overall,
        "ai_call_id": ai_call_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn session_id_is_required() {
        let c = setup();
        assert_eq!(cockpitPlanJudge(&json!({}), &c), Err("cockpit-plan-judge: session_id is required".to_string()));
        assert_eq!(cockpitPlanJudge(&json!({ "session_id": 0 }), &c), Err("cockpit-plan-judge: session_id is required".to_string()));
    }

    #[test]
    fn item_id_or_criteria_is_required_when_no_plan_pane() {
        let c = setup();
        let err = cockpitPlanJudge(&json!({ "session_id": 1 }), &c).unwrap_err();
        assert_eq!(err, "cockpit-plan-judge: item_id or criteria is required");
    }

    #[test]
    fn plan_pane_item_not_found_returns_ok_false() {
        let c = setup();
        let _ = crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c.execute(
            "INSERT INTO cockpit_blackboard (session_id, pane_id, kind, body_md) VALUES (1, 7, 'plan', ?)",
            [r#"{"phases":[{"items":[{"id":"a1","title":"t","acceptance":["x"]}]}]}"#],
        ).unwrap();
        let r = cockpitPlanJudge(&json!({ "session_id": 1, "plan_pane_id": 7, "item_id": "nope" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "item nope not found in the plan" }));
    }

    #[test]
    fn tribunal_mode_delegates_to_tribunal_twin() {
        // tribunal-history is a real, no-AI read-only mode on the tribunal twin — proves the
        // startsWith('tribunal') delegation actually routes there (not a stub/no-op).
        let c = setup();
        let r = cockpitPlanJudge(&json!({ "mode": "tribunal-history", "session_id": 5 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "sessions": [] }));
    }

    #[test]
    fn freeform_criteria_reaches_the_parity_deferred_aicall_boundary() {
        // proves every deterministic gate (guards, evidence gather, prompt-ensure) ran cleanly
        // and the error is aiCall's deferred stub, not an earlier bug.
        let c = setup();
        let err = cockpitPlanJudge(&json!({ "session_id": 9, "criteria": ["it compiles"] }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'cockpit-plan-judge'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn gather_evidence_falls_back_when_no_panes() {
        let c = setup();
        let _ = crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        let ev = gather_evidence(&c, 1).unwrap();
        assert_eq!(ev, "(no evidence yet — no worktrees, no coordination)");
    }
}
