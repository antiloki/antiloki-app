//! driver-forensics.rs — twin of skills/driver/driver-forensics.mjs (db, cross-db
//! brain.*/judgments.*/memory.*). NAMES ARE CONTRACT: default `driverForensics`. The SESSION CASE
//! FILE: deep-searches everything the substrate recorded about one pipeline run (ticks, gates, AI
//! calls, judgments incl. archived rounds, build/lint/QA gates, blueprint+snippets, burns, errors,
//! notifications), assembles a deterministic DOSSIER string + a structured `evidence` object (both
//! real, ported faithfully — this IS the reachable deterministic surface these tests exercise),
//! then op='full' calls `ensure_prompt` (reusing the already-converted `ai_prompt_find`/
//! `ai_prompt_add` twins) and ONE high-tier `crate::ai_ai_call::aiCall` to narrate the dossier.
//!
//! PARITY-DEFERRED past the first `aiCall`: the JS calls `await aiCall(...)` with NO surrounding
//! try/catch at that call site, so aiCall's rejection propagates straight out of the async
//! function — same convention as book-run.rs's `fac_call`. Because the deferred stub ALWAYS
//! returns Err (see ai-call.rs's module doc), the retry loop's second iteration, the
//! reasoning-leak salvage/guard, the `brain.driver_forensics` persistence INSERT, and the
//! `entities_risk_classify::riskClassify` auto-classify tail are never reached by any input this
//! twin can construct — they are ported below byte-for-byte from the JS (mirrors book-run.rs's own
//! documented precedent for this exact situation) but are unreachable by this file's own fixture;
//! correctness there is by inspection, not by test.
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
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
// `String(v ?? fallback)` — nullish (missing or explicit null) only, not falsy.
fn nz_str(v: Option<&Value>, fallback: &str) -> String {
    match nz(v) {
        Some(x) => js_string(x),
        None => fallback.to_string(),
    }
}
// `String(v ?? '')`
fn str_qq(v: Option<&Value>) -> String {
    nz_str(v, "")
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
fn js_num_str(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 { format!("{}", n as i64) } else { n.to_string() }
}
// `.slice(0, n)` on a JS string — approximated char-count-based (em-dash-safe).
fn take(s: &str, n: usize) -> String { s.chars().take(n).collect() }
// `.slice(-n)` — the trailing n chars.
fn last_chars(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n { s.to_string() } else { chars[chars.len() - n..].iter().collect() }
}
// `.replace(/\s+/g, ' ')`
fn collapse_ws(s: &str) -> String {
    let mut out = String::new();
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !in_ws { out.push(' '); in_ws = true; }
        } else { out.push(c); in_ws = false; }
    }
    out
}

// `q()` — swallow query errors, return [] (mirrors `try { return db.prepare(sql).all(...a); } catch { return []; }`).
fn q(db: &Connection, sql: &str, params: &[Value]) -> Vec<Value> {
    crate::db_json::query_rows(db, sql, params).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
}

const PROMPT: &str = "driver.forensics.v1";
const SYSTEM: &str = "You are the substrate's FORENSIC INVESTIGATOR. You receive a deterministic dossier of one pipeline session. You NARRATE what the evidence proves — every claim cites its [#id] marker from the dossier. You never invent events.";
const TEMPLATE: &str = "Write the forensic case report for the pipeline session below. Markdown, these sections exactly:

## THE STORY — what happened, chronologically (short paragraphs, cite [tick#] / [call#])
## FINDINGS — numbered; each ONE decidable claim + its evidence markers
## ROOT CAUSES — the deepest explanation the evidence supports (distinguish CODE vs INFRA vs PROMPT vs JUDGE causes)
## WASTE — cached-call loops, burned retries, dead-ends; cost where visible
## RECOMMENDATIONS — concrete, smallest-change-first, each tied to a finding

Rules: every claim cites [#id] markers · say \"the dossier does not show\" rather than guess · plain language.

═══ THE DOSSIER ═══
{{dossier}}";

fn ensure_prompt(db: &Connection) -> Result<(), String> {
    if let Ok(ex) = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db) {
        if ex.get("template_md").and_then(Value::as_str) == Some(TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT, "template_md": TEMPLATE, "system_md": SYSTEM,
            "default_tier": "high", "default_temperature": "cold", "max_output_tokens": 3500,
            "output_fields": [{ "field_name": "report_md", "field_type": "string" }],
            "judging_criteria": [
                "Every claim cites a [#id] dossier marker.",
                "Root causes distinguish code vs infra vs prompt vs judge.",
                "No events beyond the dossier.",
                "Recommendations are smallest-change-first and tied to findings.",
            ],
        }),
        db,
    )?;
    Ok(())
}

// recursive `flat` from the JS blueprint flattener: `${b.id}[${b.type}]`, depth-first into children.
fn flatten_blocks(blocks: &Value, out: &mut Vec<String>) {
    if let Some(arr) = blocks.as_array() {
        for b in arr {
            let id = js_string(b.get("id").unwrap_or(&Value::Null));
            let ty = js_string(b.get("type").unwrap_or(&Value::Null));
            out.push(format!("{id}[{ty}]"));
            if let Some(children) = b.get("children") {
                if !children.is_null() {
                    flatten_blocks(children, out);
                }
            }
        }
    }
}

// the dossier's QA-fail reason: `e.reason ?? (e.stderr ? \`exit ${e.exit}: ${e.stderr}\` : e.resolved_path)`,
// then `String(reason)`; on a JSON.parse failure, `String(it.evidence_md ?? '')`.
fn qa_dossier_reason(evidence_md: Option<&Value>) -> String {
    let parse_src = nz_str(evidence_md, "{}");
    match serde_json::from_str::<Value>(&parse_src) {
        Ok(e) => match nz(e.get("reason")) {
            Some(r) => js_string(r),
            None => {
                if truthy(e.get("stderr")) {
                    let exit = js_string(e.get("exit").unwrap_or(&Value::Null));
                    let stderr = js_string(e.get("stderr").unwrap_or(&Value::Null));
                    format!("exit {exit}: {stderr}")
                } else {
                    match e.get("resolved_path") {
                        None => "undefined".to_string(),
                        Some(v) => js_string(v),
                    }
                }
            }
        },
        Err(_) => str_qq(evidence_md),
    }
}

// the structured-evidence QA-fail reason: `JSON.parse(evidence_md ?? '{}')?.reason ?? ''`, then
// `reason || String(evidence_md ?? '').slice(0, 200)`.
fn qa_evidence_reason(evidence_md: Option<&Value>) -> String {
    let parse_src = nz_str(evidence_md, "{}");
    let reason_val: Value = match serde_json::from_str::<Value>(&parse_src) {
        Ok(e) => nz(e.get("reason")).cloned().unwrap_or(json!("")),
        Err(_) => json!(""),
    };
    if truthy(Some(&reason_val)) {
        js_string(&reason_val)
    } else {
        take(&str_qq(evidence_md), 200)
    }
}

pub fn driverForensics(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent). provider_fn is a JS function
    // reference threaded opaquely into aiCall's input; there is no Rust equivalent to carry (aiCall's
    // dispatch itself is the deferred boundary), so it is passed as `null` below.
    let provider_fn = Value::Null;
    let card_slug = str_qq(input.get("card_slug"));
    if card_slug.is_empty() {
        return Err("driver-forensics: card_slug is required".into());
    }
    let op = { let o = str_qq(input.get("op")); if o.is_empty() { "full".to_string() } else { o } };

    let card_rows = q(db, "SELECT slug, title, status, body_json FROM cards WHERE slug = ?", &[json!(card_slug.as_str())]);
    let Some(card) = card_rows.into_iter().next() else {
        return Ok(json!({ "ok": true, "skipped": true, "reason": format!("card '{card_slug}' not found") }));
    };

    let mut lines: Vec<String> = Vec::new();
    let card_title = js_string(card.get("title").unwrap_or(&Value::Null));
    let card_status = js_string(card.get("status").unwrap_or(&Value::Null));
    lines.push(format!("# SESSION {card_slug}"));
    lines.push(format!("card: \"{card_title}\" · status {card_status}"));

    lines.push("\n## CONTRACT CHAIN".to_string());
    for c in q(db, "SELECT slug, status, current_attempt, rejection_reason_md, worktree_path FROM contracts WHERE card_slug = ? ORDER BY id", &[json!(card_slug.as_str())]) {
        let slug = js_string(c.get("slug").unwrap_or(&Value::Null));
        let status = js_string(c.get("status").unwrap_or(&Value::Null));
        let attempt = js_string(c.get("current_attempt").unwrap_or(&Value::Null));
        let worktree_suffix = if truthy(c.get("worktree_path")) { " worktree" } else { "" };
        let rejection_suffix = match nz(c.get("rejection_reason_md")) {
            Some(r) => format!(" · rejected: {}", take(&js_string(r), 200)),
            None => String::new(),
        };
        lines.push(format!("- [contract {}] status={status} attempt={attempt}{worktree_suffix}{rejection_suffix}", last_chars(&slug, 12)));
    }

    lines.push("\n## TICKS (fired/failed only, chronological)".to_string());
    for t in q(db, "SELECT id, outcome, decided_action, decided_reason, error_message FROM brain.driver_ticks WHERE card_slug = ? AND outcome != 'idle' ORDER BY id", &[json!(card_slug.as_str())]) {
        let id = js_string(t.get("id").unwrap_or(&Value::Null));
        let outcome = js_string(t.get("outcome").unwrap_or(&Value::Null));
        let action = nz_str(t.get("decided_action"), "-");
        let reason = take(&str_qq(t.get("decided_reason")), 140);
        let err_suffix = match nz(t.get("error_message")) {
            Some(e) => format!(" · ERR: {}", take(&js_string(e), 160)),
            None => String::new(),
        };
        lines.push(format!("- [tick#{id}] {outcome} {action} — {reason}{err_suffix}"));
    }

    lines.push("\n## AI CALLS (live + cached, chronological)".to_string());
    for a in q(db, "
    SELECT a.id, p.name, a.outcome, a.outcome_class, a.cached, ROUND(COALESCE(a.cost_usd_actual,0),4) AS cost, a.tokens_out
    FROM brain.ai_calls a LEFT JOIN brain.prompt_templates p ON p.id = a.prompt_template_id
    WHERE a.session_id LIKE 'card-' || ? || '-%' ORDER BY a.id LIMIT 80", &[json!(card_slug.as_str())]) {
        let id = js_string(a.get("id").unwrap_or(&Value::Null));
        let name = nz_str(a.get("name"), "?");
        let outcome = js_string(a.get("outcome").unwrap_or(&Value::Null));
        let outcome_class = a.get("outcome_class").and_then(Value::as_str).unwrap_or("");
        let class_suffix = if !outcome_class.is_empty() && outcome_class != "clean" { format!("/{outcome_class}") } else { String::new() };
        let cached_suffix = if truthy(a.get("cached")) { " CACHED" } else { "" };
        let cost = a.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
        let tokens = nz_str(a.get("tokens_out"), "0");
        lines.push(format!("- [call#{id}] {name} {outcome}{class_suffix}{cached_suffix} ${} {tokens}tok", js_num_str(cost)));
    }

    lines.push("\n## JUDGMENTS (incl. archived rounds ::rN — overwritten history)".to_string());
    for j in q(db, "SELECT judgment_id, scope_ref, verdict, n_items, defender_rate, prosecutor_rate FROM judgments.judgment_cases WHERE card_slug = ? ORDER BY updated_at", &[json!(card_slug.as_str())]) {
        let judgment_id = j.get("judgment_id").cloned().unwrap_or(Value::Null);
        let scope_ref = js_string(j.get("scope_ref").unwrap_or(&Value::Null));
        let verdict = js_string(j.get("verdict").unwrap_or(&Value::Null));
        let def_rate = js_string(j.get("defender_rate").unwrap_or(&Value::Null));
        let pros_rate = js_string(j.get("prosecutor_rate").unwrap_or(&Value::Null));
        lines.push(format!("- [judgment {scope_ref}] {verdict} · def rate {def_rate} · pros rate {pros_rate}"));
        let items = q(db, "SELECT side, item_id, judge_value, statement, judge_why FROM judgments.judgment_items WHERE judgment_id = ? ORDER BY side DESC, item_id", &[judgment_id]);
        for it in items.into_iter().take(8) {
            let side = js_string(it.get("side").unwrap_or(&Value::Null));
            let item_id = js_string(it.get("item_id").unwrap_or(&Value::Null));
            let judged = js_string(it.get("judge_value").unwrap_or(&Value::Null));
            let statement = take(&js_string(it.get("statement").unwrap_or(&Value::Null)), 110);
            let why_suffix = match nz(it.get("judge_why")) {
                Some(w) => format!(" | why: {}", take(&js_string(w), 110)),
                None => String::new(),
            };
            lines.push(format!("  · {side} {item_id} judged={judged} — {statement}{why_suffix}"));
        }
    }

    lines.push("\n## BUILD GATES".to_string());
    for g in q(db, "SELECT id, passed, build_cmd, evidence_md FROM build_gate_runs WHERE contract_slug IN (SELECT slug FROM contracts WHERE card_slug = ?) ORDER BY id", &[json!(card_slug.as_str())]) {
        let id = js_string(g.get("id").unwrap_or(&Value::Null));
        let passed = if truthy(g.get("passed")) { "PASS" } else { "FAIL" };
        let build_cmd = js_string(g.get("build_cmd").unwrap_or(&Value::Null));
        let evidence = take(&collapse_ws(&str_qq(g.get("evidence_md"))), 220);
        lines.push(format!("- [gate#{id}] {passed} {build_cmd} — {evidence}"));
    }

    // LINT / STRUCTURAL GATES (operator 2026-07-08): the DETERMINISTIC findings — parse error +
    // line, declared-N×, missing, unfilled — a first-class forensic source. The AI narrator was
    // previously blind to these (it saw only the coarse synthetic-judgment summary), so it
    // couldn't cite WHY the deterministic gate RED, only that a judgment failed.
    lines.push("\n## LINT / STRUCTURAL GATES".to_string());
    for g in q(db, "SELECT id, mode, passed, findings_json FROM lint_gate_runs WHERE contract_slug IN (SELECT slug FROM contracts WHERE card_slug = ?) ORDER BY id", &[json!(card_slug.as_str())]) {
        let id = js_string(g.get("id").unwrap_or(&Value::Null));
        let mode = nz_str(g.get("mode"), "lint");
        let passed = truthy(g.get("passed"));
        let fs: Vec<Value> = serde_json::from_str(&nz_str(g.get("findings_json"), "[]")).unwrap_or_default();
        let errs: Vec<&Value> = fs.iter().filter(|f| f.get("severity").and_then(Value::as_str) == Some("error")).collect();
        let suffix = if !errs.is_empty() {
            let parts: Vec<String> = errs.iter().take(4).map(|f| {
                let file = nz_str(f.get("file"), "?");
                let detail = take(&collapse_ws(&str_qq(nz(f.get("detail")).or_else(|| nz(f.get("kind"))))), 160);
                format!("{file}: {detail}")
            }).collect();
            format!(" — {}", parts.join(" ; "))
        } else { String::new() };
        lines.push(format!("- [lint#{id}] {mode} {}{suffix}", if passed { "PASS" } else { "RED" }));
    }

    // QA VERIFY (acceptance) — the deterministic acceptance gate's per-AC verdicts. Operator
    // 2026-07-09: QA verify was invisible in the dossier (only build/lint/court gates showed), so
    // the narrator couldn't distinguish a malformed_ac_no_path (an unverifiable AC — a defect in
    // the AC SHAPE) from a real missing file from a failing test.
    lines.push("\n## QA VERIFY (acceptance)".to_string());
    for run in q(db, "SELECT id, acceptance_pass, acceptance_fail, all_passed FROM qa_runs WHERE contract_slug IN (SELECT slug FROM contracts WHERE card_slug = ?) ORDER BY id", &[json!(card_slug.as_str())]) {
        let run_id = run.get("id").cloned().unwrap_or(Value::Null);
        let passed = if truthy(run.get("all_passed")) { "PASS" } else { "FAIL" };
        let acc_pass = nz_str(run.get("acceptance_pass"), "?");
        let acc_fail = nz_str(run.get("acceptance_fail"), "?");
        lines.push(format!("- [qa#{}] {passed} — {acc_pass} pass / {acc_fail} fail", js_string(&run_id)));
        let results = q(db, "SELECT ac_index, kind, evidence_md FROM qa_run_results WHERE qa_run_id = ? AND passed = 0 ORDER BY ac_index", &[run_id]);
        for it in results.into_iter().take(10) {
            let ac_index = it.get("ac_index").and_then(Value::as_i64).unwrap_or(0) + 1;
            let kind = js_string(it.get("kind").unwrap_or(&Value::Null));
            let reason = take(&collapse_ws(&qa_dossier_reason(it.get("evidence_md"))), 180);
            lines.push(format!("  · AC#{ac_index} ({kind}) FAIL — {reason}"));
        }
    }

    // BLUEPRINT + STUBS + AUTHORED CODE (operator 2026-07-08: "add the diffs, the stubs, the
    // blueprint — anything that helps"). The forensic can now see WHAT DEV actually produced, not
    // just that a gate REDed — a TODO-stub snippet vs a real file is the difference between a
    // truncated author call and a code defect.
    lines.push("\n## BLUEPRINT + AUTHORED CODE (what DEV produced)".to_string());
    let bp_rows = q(db, "SELECT id, version, blueprint_json FROM blueprints WHERE contract_slug IN (SELECT slug FROM contracts WHERE card_slug = ?) AND superseded_by IS NULL ORDER BY id DESC LIMIT 1", &[json!(card_slug.as_str())]);
    if let Some(bp_row) = bp_rows.into_iter().next() {
        let bp_id = js_string(bp_row.get("id").unwrap_or(&Value::Null));
        let bp_version = js_string(bp_row.get("version").unwrap_or(&Value::Null));
        if let Ok(parsed) = serde_json::from_str::<Value>(&str_qq(bp_row.get("blueprint_json"))) {
            let mut flat: Vec<String> = Vec::new();
            flatten_blocks(parsed.get("blocks").unwrap_or(&Value::Null), &mut flat);
            let joined = flat.into_iter().take(24).collect::<Vec<_>>().join(" ");
            lines.push(format!("- [blueprint#{bp_id} v{bp_version}] {joined}"));
            if let Some(stub_files) = parsed.get("stub_files").and_then(Value::as_array) {
                for sf in stub_files.iter().take(6) {
                    let path = js_string(sf.get("path").unwrap_or(&Value::Null));
                    let source = take(&collapse_ws(&str_qq(sf.get("source"))), 140);
                    lines.push(format!("  · [stub {path}] {source}"));
                }
            }
        }
        let bp_id_param = bp_row.get("id").cloned().unwrap_or(Value::Null);
        for sn in q(db, "SELECT block_id, LENGTH(code) AS len, code FROM blueprint_snippets WHERE blueprint_id = ?", &[bp_id_param]) {
            let block_id = js_string(sn.get("block_id").unwrap_or(&Value::Null));
            let len = js_string(sn.get("len").unwrap_or(&Value::Null));
            let head = take(&collapse_ws(&str_qq(sn.get("code"))), 200);
            lines.push(format!("  · [snippet {block_id}] ({len} chars) {head}"));
        }
    } else {
        lines.push("- (no active blueprint for this card)".to_string());
    }

    lines.push("\n## BURNS + ERRORS + NOTIFICATIONS".to_string());
    let burns_rows = q(db, "SELECT COUNT(*) AS n FROM brain.heimdall_evidence e JOIN brain.ai_calls a ON a.id = e.ai_call_id WHERE e.outcome='burn' AND a.session_id LIKE 'card-' || ? || '-%'", &[json!(card_slug.as_str())]);
    let burns: i64 = burns_rows.first().and_then(|r| r.get("n")).and_then(Value::as_i64).unwrap_or(0);
    lines.push(format!("- burn evidence rows: {burns}"));
    for e in q(db, "SELECT error_code, error_message, pattern_key FROM memory.error_events WHERE related_card_slug = ? ORDER BY id DESC LIMIT 8", &[json!(card_slug.as_str())]) {
        // `e.pattern_key ?? e.error_code` — nullish only, no further fallback (both-null → "null").
        let key = match nz(e.get("pattern_key")) {
            Some(v) => js_string(v),
            None => js_string(e.get("error_code").unwrap_or(&Value::Null)),
        };
        let msg = take(&str_qq(e.get("error_message")), 160);
        lines.push(format!("- [error {key}] {msg}"));
    }
    for note in q(db, "SELECT id, severity, message FROM notifications WHERE related_card_slug = ? ORDER BY id DESC LIMIT 6", &[json!(card_slug.as_str())]) {
        let id = js_string(note.get("id").unwrap_or(&Value::Null));
        let severity = js_string(note.get("severity").unwrap_or(&Value::Null));
        let msg = take(&js_string(note.get("message").unwrap_or(&Value::Null)), 160);
        lines.push(format!("- [notif#{id}] {severity}: {msg}"));
    }

    let dossier_md = take(&lines.join("\n"), 14000);

    // STRUCTURED EVIDENCE (operator 2026-07-09): the case's errors gathered as DATA (not markdown),
    // so the forensic window can render an organized "Evidence" tab — every failure with its real
    // detail. Same sources as the dossier, filtered to what FAILED. Deterministic + free (no AI).
    let escalations: Vec<Value> = q(db, "SELECT id, severity, message FROM notifications WHERE related_card_slug = ? AND severity IN ('warn','error') ORDER BY id DESC LIMIT 10", &[json!(card_slug.as_str())])
        .into_iter().map(|nrow| json!({
            "id": nrow.get("id").cloned().unwrap_or(Value::Null),
            "severity": nrow.get("severity").cloned().unwrap_or(Value::Null),
            "message": take(&str_qq(nrow.get("message")), 700),
        })).collect();
    let mut judgment_defects: Vec<Value> = Vec::new();
    let mut gate_reds: Vec<Value> = Vec::new();
    let mut qa_fails: Vec<Value> = Vec::new();
    let contract_rejections: Vec<Value> = q(db, "SELECT slug, current_attempt, rejection_reason_md FROM contracts WHERE card_slug = ? AND rejection_reason_md IS NOT NULL ORDER BY id", &[json!(card_slug.as_str())])
        .into_iter().map(|c| json!({
            "contract": last_chars(&js_string(c.get("slug").unwrap_or(&Value::Null)), 14),
            "attempt": c.get("current_attempt").cloned().unwrap_or(Value::Null),
            "reason": take(&js_string(c.get("rejection_reason_md").unwrap_or(&Value::Null)), 800),
        })).collect();
    let failed_ticks: Vec<Value> = q(db, "SELECT id, outcome, decided_action, decided_reason, error_message FROM brain.driver_ticks WHERE card_slug = ? AND (outcome IN ('failed','escalated','halt_requested') OR error_message IS NOT NULL) ORDER BY id", &[json!(card_slug.as_str())])
        .into_iter().map(|t| json!({
            "id": t.get("id").cloned().unwrap_or(Value::Null),
            "outcome": t.get("outcome").cloned().unwrap_or(Value::Null),
            "action": nz_str(t.get("decided_action"), "-"),
            "detail": take(&nz_str(t.get("error_message"), &str_qq(t.get("decided_reason"))), 500),
        })).collect();
    let error_events: Vec<Value> = q(db, "SELECT id, error_code, error_message, pattern_key, recovery_outcome FROM memory.error_events WHERE related_card_slug = ? ORDER BY id DESC LIMIT 10", &[json!(card_slug.as_str())])
        .into_iter().map(|e| json!({
            "id": e.get("id").cloned().unwrap_or(Value::Null),
            "code": nz_str(e.get("pattern_key"), &nz_str(e.get("error_code"), "error")),
            "message": take(&str_qq(e.get("error_message")), 400),
            "recovered": truthy(e.get("recovery_outcome")),
        })).collect();
    let ai_errors: Vec<Value> = q(db, "SELECT a.id, p.name, a.outcome, a.outcome_class, a.model_id FROM brain.ai_calls a LEFT JOIN brain.prompt_templates p ON p.id = a.prompt_template_id WHERE a.session_id LIKE 'card-' || ? || '-%' AND a.outcome != 'success' ORDER BY a.id", &[json!(card_slug.as_str())])
        .into_iter().map(|a| json!({
            "id": a.get("id").cloned().unwrap_or(Value::Null),
            "name": nz_str(a.get("name"), "?"),
            "class": nz_str(a.get("outcome_class"), &js_string(a.get("outcome").unwrap_or(&Value::Null))),
            "model": nz_str(a.get("model_id"), "?"),
        })).collect();

    for j in q(db, "SELECT judgment_id, scope_ref, verdict FROM judgments.judgment_cases WHERE card_slug = ? AND verdict = 'fail' ORDER BY updated_at", &[json!(card_slug.as_str())]) {
        let judgment_id = j.get("judgment_id").cloned().unwrap_or(Value::Null);
        let defects: Vec<Value> = q(db, "SELECT statement, judge_why FROM judgments.judgment_items WHERE judgment_id = ? AND side='prosecutor' AND judge_value = 1", &[judgment_id])
            .into_iter().map(|it| json!({
                "statement": take(&str_qq(it.get("statement")), 400),
                "why": take(&str_qq(it.get("judge_why")), 400),
            })).collect();
        if !defects.is_empty() {
            judgment_defects.push(json!({ "scope": j.get("scope_ref").cloned().unwrap_or(Value::Null), "defects": defects }));
        }
    }
    for g in q(db, "SELECT id, evidence_md FROM build_gate_runs WHERE contract_slug IN (SELECT slug FROM contracts WHERE card_slug = ?) AND passed = 0 AND COALESCE(skipped,0) = 0 ORDER BY id", &[json!(card_slug.as_str())]) {
        gate_reds.push(json!({
            "kind": "build",
            "id": g.get("id").cloned().unwrap_or(Value::Null),
            "detail": take(&collapse_ws(&str_qq(g.get("evidence_md"))), 600),
        }));
    }
    for g in q(db, "SELECT id, mode, findings_json FROM lint_gate_runs WHERE contract_slug IN (SELECT slug FROM contracts WHERE card_slug = ?) AND passed = 0 ORDER BY id", &[json!(card_slug.as_str())]) {
        let mode = nz_str(g.get("mode"), "lint");
        let fs: Vec<Value> = serde_json::from_str(&nz_str(g.get("findings_json"), "[]")).unwrap_or_default();
        let errs: Vec<&Value> = fs.iter().filter(|f| f.get("severity").and_then(Value::as_str) == Some("error")).collect();
        let chosen: Vec<&Value> = if !errs.is_empty() { errs } else { fs.iter().collect() };
        let detail = take(&chosen.into_iter().take(5).map(|f| format!("{}: {}", nz_str(f.get("file"), "?"), str_qq(nz(f.get("detail")).or_else(|| nz(f.get("kind")))))).collect::<Vec<_>>().join("  ·  "), 700);
        gate_reds.push(json!({ "kind": format!("lint · {mode}"), "id": g.get("id").cloned().unwrap_or(Value::Null), "detail": detail }));
    }
    for run in q(db, "SELECT id, feedback_md FROM qa_runs WHERE contract_slug IN (SELECT slug FROM contracts WHERE card_slug = ?) AND COALESCE(all_passed,1) = 0 ORDER BY id", &[json!(card_slug.as_str())]) {
        let run_id = run.get("id").cloned().unwrap_or(Value::Null);
        let per_ac = q(db, "SELECT ac_index, kind, evidence_md FROM qa_run_results WHERE qa_run_id = ? AND passed = 0 ORDER BY ac_index", &[run_id]);
        if !per_ac.is_empty() {
            for it in per_ac.into_iter().take(12) {
                qa_fails.push(json!({
                    "ac_index": it.get("ac_index").and_then(Value::as_i64).unwrap_or(0) + 1,
                    "kind": it.get("kind").cloned().unwrap_or(Value::Null),
                    "reason": qa_evidence_reason(it.get("evidence_md")),
                }));
            }
        } else {
            // QA FAILED with NO per-AC failure — a SCOPE or SENIOR-REVIEW fail. The gate persists the
            // real reason to feedback_md; surface it so the evidence is never blank.
            static QA_BANNER_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
            let re = QA_BANNER_RE.get_or_init(|| regex::Regex::new(r"(?i)===\s*QA GATE FAILED\s*===").unwrap());
            let fb = collapse_ws(&re.replace_all(&str_qq(run.get("feedback_md")), "")).trim().to_string();
            let reason = if !fb.is_empty() { take(&fb, 400) } else { "QA gate failed with no recorded detail (scope / senior review) — investigate.".to_string() };
            qa_fails.push(json!({ "ac_index": Value::Null, "kind": "gate", "reason": reason }));
        }
    }

    let judgment_defects_total: usize = judgment_defects.iter().map(|j| j.get("defects").and_then(Value::as_array).map_or(0, |a| a.len())).sum();
    let total = escalations.len() + judgment_defects_total + gate_reds.len() + qa_fails.len()
        + contract_rejections.len() + failed_ticks.len() + error_events.len() + ai_errors.len();

    let mut evidence_obj = Map::new();
    evidence_obj.insert("escalations".into(), json!(escalations));
    evidence_obj.insert("judgment_defects".into(), json!(judgment_defects));
    evidence_obj.insert("gate_reds".into(), json!(gate_reds));
    evidence_obj.insert("qa_fails".into(), json!(qa_fails));
    evidence_obj.insert("contract_rejections".into(), json!(contract_rejections));
    evidence_obj.insert("failed_ticks".into(), json!(failed_ticks));
    evidence_obj.insert("error_events".into(), json!(error_events));
    evidence_obj.insert("ai_errors".into(), json!(ai_errors));
    evidence_obj.insert("total".into(), json!(total));
    let evidence = Value::Object(evidence_obj);

    // per-source counts — the investigation window's REAL step list (never theater)
    let count = |sql: &str, params: &[Value]| -> i64 {
        q(db, sql, params).first().and_then(|r| r.get("n")).and_then(Value::as_i64).unwrap_or(0)
    };
    let stats = json!({
        "contracts": count("SELECT COUNT(*) AS n FROM contracts WHERE card_slug = ?", &[json!(card_slug.as_str())]),
        "ticks": count("SELECT COUNT(*) AS n FROM brain.driver_ticks WHERE card_slug = ? AND outcome != 'idle'", &[json!(card_slug.as_str())]),
        "calls": count("SELECT COUNT(*) AS n FROM brain.ai_calls WHERE session_id LIKE 'card-' || ? || '-%'", &[json!(card_slug.as_str())]),
        "cached": count("SELECT COUNT(*) AS n FROM brain.ai_calls WHERE session_id LIKE 'card-' || ? || '-%' AND cached = 1", &[json!(card_slug.as_str())]),
        "judgments": count("SELECT COUNT(*) AS n FROM judgments.judgment_cases WHERE card_slug = ?", &[json!(card_slug.as_str())]),
        "gates": count("SELECT COUNT(*) AS n FROM build_gate_runs WHERE contract_slug IN (SELECT slug FROM contracts WHERE card_slug = ?)", &[json!(card_slug.as_str())]),
        "lint_gates": count("SELECT COUNT(*) AS n FROM lint_gate_runs WHERE contract_slug IN (SELECT slug FROM contracts WHERE card_slug = ?)", &[json!(card_slug.as_str())]),
        "qa_runs": count("SELECT COUNT(*) AS n FROM qa_runs WHERE contract_slug IN (SELECT slug FROM contracts WHERE card_slug = ?)", &[json!(card_slug.as_str())]),
        "errors": count("SELECT COUNT(*) AS n FROM memory.error_events WHERE related_card_slug = ?", &[json!(card_slug.as_str())]),
        "burns": burns,
        "notifications": count("SELECT COUNT(*) AS n FROM notifications WHERE related_card_slug = ?", &[json!(card_slug.as_str())]),
    });

    if op == "dossier" {
        return Ok(json!({ "ok": true, "dossier_md": dossier_md, "stats": stats, "evidence": evidence }));
    }
    ensure_prompt(db)?;

    // REASONING-LEAK GUARD (r25/r31/r36) — PARITY-DEFERRED past this point: the JS calls `await
    // aiCall(...)` with no try/catch at this exact call site, so the deferred stub's Err propagates
    // straight out via `?` below on the FIRST attempt — attempt 2, the salvage/degenerate-tagging,
    // the persistence INSERT, and the auto-classify tail are ported faithfully (byte-for-byte from
    // the JS) but are unreachable by any input this twin can construct.
    static STORY_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static FINDINGS_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static DELIBERATING_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let story_re = STORY_RE.get_or_init(|| regex::Regex::new(r"(?im)^##\s*THE STORY").unwrap());
    let findings_re = FINDINGS_RE.get_or_init(|| regex::Regex::new(r"(?im)^##\s*FINDINGS").unwrap());
    let deliberating_re = DELIBERATING_RE.get_or_init(|| regex::Regex::new(r"(We need|Let'?s (compute|sum|list|check)|Need (sections|findings|maybe|cite|perhaps)|Is that acceptable|Probably\.)").unwrap());

    let mut res: Option<Value> = None;
    let mut report_md = "(empty report)".to_string();
    for attempt in 1..=2 {
        let mut input_kv = vec![json!({ "key": "dossier", "value": dossier_md })];
        if attempt > 1 {
            input_kv.push(json!({ "key": "retry_note", "value": format!("retry {attempt}: the previous narration lacked the mandatory sections — output ONLY the report, starting at \"## THE STORY\".") }));
        }
        let ai_input = json!({
            "prompt_name": PROMPT, "input_kv": input_kv, "tier": "high", "temperature": "cold",
            "phase_flow": "single_only", "two_phase": false, "structured_output": false, "provider_fn": provider_fn,
        });
        let r = crate::ai_ai_call::aiCall(&ai_input, db)?;
        report_md = r.get("output_kv").and_then(Value::as_array).and_then(|arr| arr.iter().find(|o| nz(o.get("value_text")).is_some())).and_then(|o| o.get("value_text")).map(js_string).unwrap_or_else(|| "(empty report)".to_string());
        res = Some(r);
        if let Some(m) = story_re.find(&report_md) {
            if m.start() > 0 { report_md = report_md[m.start()..].to_string(); }
        }
        let has_headings = story_re.is_match(&report_md) && findings_re.is_match(&report_md);
        let head = take(&report_md, 1200);
        let deliberating = deliberating_re.is_match(&head);
        if has_headings && !deliberating { break; }
        let ai_call_id = res.as_ref().and_then(|r| r.get("ai_call_id")).cloned().unwrap_or(Value::Null);
        let _ = crate::ai_ai_outcome_report::aiOutcomeReport(&json!({ "ai_call_id": ai_call_id, "outcome_class": "degenerate" }), db);
        if attempt == 2 {
            report_md = format!("## NARRATION FAILED\nThe investigator model returned deliberation text instead of the sectioned report, twice. The deterministic dossier below is complete and citable — re-run the forensic to draw another model.\n\n---\n\n{}", take(&report_md, 1500));
        }
    }

    let ai_call_id = res.as_ref().and_then(|r| r.get("ai_call_id")).cloned().unwrap_or(Value::Null);
    let mut forensic_id: Value = Value::Null;
    let create_ok = db.execute_batch("CREATE TABLE IF NOT EXISTS brain.driver_forensics (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT NOT NULL, ts TEXT NOT NULL DEFAULT (datetime('now')), report_md TEXT, dossier_md TEXT, forensic_ai_call_id INTEGER)").is_ok();
    if create_ok {
        if let Ok((_changes, rowid)) = crate::db_json::run_write(
            db,
            "INSERT INTO brain.driver_forensics (card_slug, report_md, dossier_md, forensic_ai_call_id) VALUES (?,?,?,?)",
            &[json!(card_slug.as_str()), json!(report_md), json!(dossier_md), ai_call_id.clone()],
        ) {
            forensic_id = json!(rowid);
        }
    }

    // EPIC-ERROR-CONTROL-CENTER: auto-classify NEW recurring failure shape after each forensic —
    // ONE high-tier call, and only when a recurring pattern still has no risk tag. Additive +
    // degrade-safe (never breaks the forensic) + kill-switch.
    let mut auto_classified: Value = Value::Null;
    if std::env::var("ANTILOKI_AUTO_RISK_CLASSIFY").ok().as_deref() != Some("off") {
        if let Ok(rc) = crate::entities_risk_classify::riskClassify(&json!({ "only_unclassified": true, "min_count": 2 }), db) {
            auto_classified = nz(rc.get("classified")).cloned().unwrap_or(json!(0));
        }
    }

    Ok(json!({
        "ok": true, "report_md": report_md, "dossier_md": dossier_md, "stats": stats, "evidence": evidence,
        "forensic_ai_call_id": ai_call_id, "forensic_id": forensic_id, "auto_classified": auto_classified,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS memory;").unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, body_json TEXT);
             CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL, card_slug TEXT NOT NULL, status TEXT, current_attempt INTEGER, rejection_reason_md TEXT, worktree_path TEXT);
             CREATE TABLE build_gate_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, passed INTEGER, skipped INTEGER, build_cmd TEXT, evidence_md TEXT);
             CREATE TABLE lint_gate_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, mode TEXT, passed INTEGER, findings_json TEXT);
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, acceptance_pass INTEGER, acceptance_fail INTEGER, all_passed INTEGER, feedback_md TEXT);
             CREATE TABLE qa_run_results (id INTEGER PRIMARY KEY AUTOINCREMENT, qa_run_id INTEGER, ac_index INTEGER, kind TEXT, evidence_md TEXT, passed INTEGER);
             CREATE TABLE blueprints (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, version INTEGER, blueprint_json TEXT, superseded_by INTEGER);
             CREATE TABLE blueprint_snippets (id INTEGER PRIMARY KEY AUTOINCREMENT, blueprint_id INTEGER, block_id TEXT, code TEXT);
             CREATE TABLE notifications (id INTEGER PRIMARY KEY AUTOINCREMENT, related_card_slug TEXT, severity TEXT, message TEXT);
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, outcome TEXT, decided_action TEXT, decided_reason TEXT, error_message TEXT);
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT, prompt_template_id INTEGER, outcome TEXT, outcome_class TEXT, cached INTEGER, cost_usd_actual REAL, tokens_out INTEGER, model_id TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.heimdall_evidence (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, outcome TEXT);
             CREATE TABLE judgments.judgment_cases (judgment_id TEXT PRIMARY KEY, card_slug TEXT, scope_ref TEXT, verdict TEXT, n_items INTEGER, defender_rate REAL, prosecutor_rate REAL, updated_at TEXT);
             CREATE TABLE judgments.judgment_items (id INTEGER PRIMARY KEY AUTOINCREMENT, judgment_id TEXT, side TEXT, item_id TEXT, judge_value INTEGER, statement TEXT, judge_why TEXT);
             CREATE TABLE memory.error_events (id INTEGER PRIMARY KEY AUTOINCREMENT, related_card_slug TEXT, error_code TEXT, error_message TEXT, pattern_key TEXT, recovery_outcome TEXT);",
        ).unwrap();
        c
    }

    #[test]
    fn requires_card_slug() {
        let c = setup();
        assert!(driverForensics(&json!({}), &c).unwrap_err().contains("card_slug is required"));
    }

    #[test]
    fn skips_when_card_not_found() {
        let c = setup();
        let r = driverForensics(&json!({ "card_slug": "nope" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["skipped"], json!(true));
        assert_eq!(r["reason"], json!("card 'nope' not found"));
    }

    #[test]
    fn dossier_op_returns_deterministic_shape_without_calling_ai() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, title, status) VALUES ('c1', 'The Card', 'in_progress')", []).unwrap();
        c.execute("INSERT INTO contracts (slug, card_slug, status, current_attempt, worktree_path) VALUES ('contract-abc123456789', 'c1', 'active', 2, '/tmp/wt')", []).unwrap();
        c.execute("INSERT INTO brain.driver_ticks (card_slug, outcome, decided_action, decided_reason) VALUES ('c1', 'fired', 'advance', 'gate passed')", []).unwrap();
        c.execute("INSERT INTO notifications (related_card_slug, severity, message) VALUES ('c1', 'warn', 'careful')", []).unwrap();
        let r = driverForensics(&json!({ "card_slug": "c1", "op": "dossier" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let dossier = r["dossier_md"].as_str().unwrap();
        assert!(dossier.contains("# SESSION c1"));
        assert!(dossier.contains("[contract abc123456789] status=active attempt=2 worktree"));
        assert!(dossier.contains("[tick#1] fired advance — gate passed"));
        assert_eq!(r["stats"]["contracts"], json!(1));
        assert_eq!(r["stats"]["ticks"], json!(1));
        assert_eq!(r["stats"]["notifications"], json!(1));
        assert_eq!(r["evidence"]["escalations"][0]["message"], json!("careful"));
        assert_eq!(r["evidence"]["total"], json!(1));
    }

    #[test]
    fn qa_dossier_reason_uses_reason_then_stderr_then_resolved_path() {
        assert_eq!(qa_dossier_reason(Some(&json!("{\"reason\":\"file missing\"}"))), "file missing");
        assert_eq!(qa_dossier_reason(Some(&json!("{\"stderr\":\"boom\",\"exit\":1}"))), "exit 1: boom");
        assert_eq!(qa_dossier_reason(Some(&json!("{\"resolved_path\":\"/a/b\"}"))), "/a/b");
        assert_eq!(qa_dossier_reason(Some(&json!("not json"))), "not json");
        assert_eq!(qa_dossier_reason(None), "undefined");
    }

    #[test]
    fn qa_verify_section_and_evidence_gate_reds_populate_from_failed_runs() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, title, status) VALUES ('c2', 'Card2', 'review')", []).unwrap();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('contract-2', 'c2')", []).unwrap();
        c.execute("INSERT INTO qa_runs (contract_slug, acceptance_pass, acceptance_fail, all_passed) VALUES ('contract-2', 1, 1, 0)", []).unwrap();
        let run_id = c.last_insert_rowid();
        c.execute("INSERT INTO qa_run_results (qa_run_id, ac_index, kind, evidence_md, passed) VALUES (?, 0, 'file', '{\"reason\":\"missing file\"}', 0)", [run_id]).unwrap();
        c.execute("INSERT INTO build_gate_runs (contract_slug, passed, skipped, build_cmd, evidence_md) VALUES ('contract-2', 0, 0, 'npm test', 'boom   failed')", []).unwrap();
        let r = driverForensics(&json!({ "card_slug": "c2", "op": "dossier" }), &c).unwrap();
        let dossier = r["dossier_md"].as_str().unwrap();
        assert!(dossier.contains("AC#1 (file) FAIL — missing file"));
        assert!(dossier.contains("[gate#1] FAIL npm test — boom failed"));
        assert_eq!(r["evidence"]["qa_fails"][0]["reason"], json!("missing file"));
        assert_eq!(r["evidence"]["qa_fails"][0]["ac_index"], json!(1));
        assert_eq!(r["evidence"]["gate_reds"][0]["kind"], json!("build"));
    }

    #[test]
    fn qa_scope_fail_with_no_per_ac_rows_surfaces_feedback_banner() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, title, status) VALUES ('c3', 'Card3', 'review')", []).unwrap();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('contract-3', 'c3')", []).unwrap();
        c.execute("INSERT INTO qa_runs (contract_slug, all_passed, feedback_md) VALUES ('contract-3', 0, '=== QA GATE FAILED ===   scope check failed')", []).unwrap();
        let r = driverForensics(&json!({ "card_slug": "c3", "op": "dossier" }), &c).unwrap();
        assert_eq!(r["evidence"]["qa_fails"][0]["ac_index"], Value::Null);
        assert_eq!(r["evidence"]["qa_fails"][0]["reason"], json!("scope check failed"));
    }

    #[test]
    fn blueprint_flattens_blocks_and_stub_files_and_snippets() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, title, status) VALUES ('c4', 'Card4', 'review')", []).unwrap();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('contract-4', 'c4')", []).unwrap();
        let bp = json!({
            "blocks": [{ "id": "b1", "type": "component", "children": [{ "id": "b1a", "type": "handler" }] }],
            "stub_files": [{ "path": "src/x.ts", "source": "// TODO   implement" }],
        });
        c.execute("INSERT INTO blueprints (contract_slug, version, blueprint_json) VALUES ('contract-4', 1, ?)", [bp.to_string()]).unwrap();
        let bp_id = c.last_insert_rowid();
        c.execute("INSERT INTO blueprint_snippets (blueprint_id, block_id, code) VALUES (?, 'b1', 'fn x() {}')", [bp_id]).unwrap();
        let r = driverForensics(&json!({ "card_slug": "c4", "op": "dossier" }), &c).unwrap();
        let dossier = r["dossier_md"].as_str().unwrap();
        assert!(dossier.contains("[blueprint#1 v1] b1[component] b1a[handler]"));
        assert!(dossier.contains("[stub src/x.ts] // TODO implement"));
        assert!(dossier.contains("[snippet b1] (9 chars) fn x() {}"));
    }

    #[test]
    fn no_active_blueprint_line_when_none_exists() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, title, status) VALUES ('c5', 'Card5', 'review')", []).unwrap();
        let r = driverForensics(&json!({ "card_slug": "c5", "op": "dossier" }), &c).unwrap();
        assert!(r["dossier_md"].as_str().unwrap().contains("- (no active blueprint for this card)"));
    }

    #[test]
    fn op_full_reaches_the_parity_deferred_aicall_boundary() {
        // proves every deterministic gate (dossier assembly, evidence, stats, ensure_prompt) ran
        // cleanly and the error is aiCall's deferred stub, not an earlier bug.
        let c = setup();
        c.execute("INSERT INTO cards (slug, title, status) VALUES ('c6', 'Card6', 'review')", []).unwrap();
        let err = driverForensics(&json!({ "card_slug": "c6" }), &c).unwrap_err();
        assert!(err.contains("ai-call") || err.contains("ai-call twin"), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'driver.forensics.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }
}
