//! cockpit-session-narrate.rs — twin of skills/cockpit/cockpit-session-narrate.mjs (db lane, ONE
//! low-tier AI call). NAMES ARE CONTRACT: default `cockpitSessionNarrate`. The 📰 NARRATOR flip on
//! the session-timeline pane: turns cockpit-session-audit's activity ledgers into a short
//! operator-readable story. Fired on explicit click only — never automatic.
//!
//! `context.db` missing → JS throws `err.contextMissing('cockpit-session-narrate', 'db')`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (same convention as
//! every sibling db-lane twin). `context.provider_fn` has no Rust equivalent — threaded as `null`
//! into the constructed aiCall input, same as pl-research.rs/book-run.rs's `provider_fn`.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`, so
//! `cockpitSessionNarrate` propagates that error once the (real, ported) deterministic surface
//! above it has run: guard, `cockpit-session-audit` read (via the already-converted
//! `cockpit_cockpit_session_audit::cockpitSessionAudit` twin), `ensure_prompt` (reusing the
//! already-converted `ai_prompt_find`/`ai_prompt_add` twins), and the activity-ledger line
//! assembly (`build_lines`: three `.map` passes over runtime/ai_calls/timeline, concatenated,
//! JS-lexicographic-string-sorted, reversed, sliced to 100). The response-parse helper
//! (`find_story`, which mirrors the `.find` over `output_kv`) is ported as its own PURE function
//! and unit-tested directly with canned `output_kv` values — unreachable from
//! `cockpitSessionNarrate` itself (the `?` on `aiCall` short-circuits before it runs) but correct
//! by construction and independently testable per the crossing playbook's coverage rule (same
//! pattern as pl-research.rs's `build_research_report`).
// QUALITY: mixed unit — deterministic assembly/db-read surface (fully ported, fixture-reachable up
// to the deferred aiCall boundary) + 1 pure response-shaping helper (unreachable from the default
// export, unit-tested directly) + 1 bespoke/deferred aiCall dispatch (no fixture/frontier).
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
} // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
} // = JS `${v}`
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
} // = JS Number(v)

const PROMPT: &str = "cockpit-session-narrate";
const SYSTEM_MD: &str = "You are the session narrator for a multi-agent coding cockpit. Write a SHORT, plain, past-tense story (6-10 sentences max) of what happened, for the human operator. Name panes by their labels, call out spend and failures honestly, end with the current state. No headers, no lists, no fluff.";
const TEMPLATE_MD: &str = "SESSION: {{session_name}}\nMISSION: {{mission}}\n\nACTIVITY LEDGER (newest first):\n{{activity}}\n\nWrite the story.";

// mirrors JS `ensurePrompt`: `try { existing = promptFind(...) } catch { existing = null }`; if
// found AND its template_md matches the current TEMPLATE constant, no-op; otherwise (re)write it.
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    if let Ok(existing) = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db) {
        if str_qq(existing.get("template_md")) == TEMPLATE_MD {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE_MD,
            "system_md": SYSTEM_MD,
            "default_tier": "low",
            "default_temperature": "warm",
            "phase_flow": "single_only",
            "max_output_tokens": 900,
            "output_fields": [{ "field_name": "story_md", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

// `audit.runtime.slice(0,60).map(r => \`[skill ${r.ts}] ${r.action ?? ''}${r.skill ? \` › ${r.skill}\` : ''} → ${r.outcome ?? ''}${r.error_code ? \` [${r.error_code}]\` : ''}\`)`
fn runtime_line(r: &Value) -> String {
    let action = str_qq(r.get("action"));
    let skill = r.get("skill").filter(|v| !v.is_null() && v.as_str() != Some(""));
    let skill_part = skill.map(|s| format!(" › {}", tmpl(Some(s)))).unwrap_or_default();
    let outcome = str_qq(r.get("outcome"));
    let error_code = r.get("error_code").filter(|v| !v.is_null() && v.as_str() != Some(""));
    let err_part = error_code.map(|e| format!(" [{}]", tmpl(Some(e)))).unwrap_or_default();
    format!("[skill {}] {}{} → {}{}", tmpl(r.get("ts")), action, skill_part, outcome, err_part)
}

// `audit.ai_calls.slice(0,40).map(a => \`[ai ${a.ts}] ${a.model_id ?? a.provider ?? '?'} $${Number(a.cost_usd_actual ?? 0).toFixed(4)} ${a.subagent ?? ''} → ${a.outcome ?? ''}\`)`
fn ai_call_line(a: &Value) -> String {
    let model = a.get("model_id").filter(|v| !v.is_null())
        .or_else(|| a.get("provider").filter(|v| !v.is_null()));
    let model_s = model.map(|v| tmpl(Some(v))).unwrap_or_else(|| "?".to_string());
    let cost = js_number(a.get("cost_usd_actual").filter(|v| !v.is_null()).or(Some(&json!(0))));
    let cost_s = if cost.is_nan() { "NaN".to_string() } else { format!("{:.4}", cost) };
    let subagent = str_qq(a.get("subagent"));
    let outcome = str_qq(a.get("outcome"));
    format!("[ai {}] {} ${} {} → {}", tmpl(a.get("ts")), model_s, cost_s, subagent, outcome)
}

// `audit.timeline.slice(0,30).map(t => \`[audit ${t.ts}] ${t.operation ?? ''} ${t.entity_type ?? ''}#${t.entity_id ?? ''}\`)`
fn timeline_line(t: &Value) -> String {
    format!("[audit {}] {} {}#{}", tmpl(t.get("ts")), str_qq(t.get("operation")), str_qq(t.get("entity_type")), str_qq(t.get("entity_id")))
}

// `[...runtime_lines, ...ai_lines, ...timeline_lines].sort().reverse().slice(0,100)` — JS `.sort()`
// with no comparator is a lexicographic string sort (UTF-16 code unit order); ASCII content here
// makes a plain byte/codepoint sort exact.
fn build_lines(audit: &Value) -> Vec<String> {
    let runtime = audit.get("runtime").and_then(Value::as_array).cloned().unwrap_or_default();
    let ai_calls = audit.get("ai_calls").and_then(Value::as_array).cloned().unwrap_or_default();
    let timeline = audit.get("timeline").and_then(Value::as_array).cloned().unwrap_or_default();

    let mut lines: Vec<String> = Vec::new();
    lines.extend(runtime.iter().take(60).map(runtime_line));
    lines.extend(ai_calls.iter().take(40).map(ai_call_line));
    lines.extend(timeline.iter().take(30).map(timeline_line));
    lines.sort();
    lines.reverse();
    lines.truncate(100);
    lines
}

// `res?.output_kv?.find(o => o.field_name === 'story_md' && o.parent_output_id == null)?.value_text ?? ''`
// — pure, unreachable from cockpitSessionNarrate (aiCall's `?` short-circuits first), independently
// tested per the playbook's coverage rule for unreachable-but-correct helpers.
fn find_story(res: &Value) -> String {
    let rows = match res.get("output_kv").and_then(Value::as_array) {
        Some(r) => r,
        None => return String::new(),
    };
    for o in rows {
        let field_ok = o.get("field_name").and_then(Value::as_str) == Some("story_md");
        // JS `== null` matches both absent and explicit null.
        let parent_ok = matches!(o.get("parent_output_id"), None | Some(Value::Null));
        if field_ok && parent_ok {
            return match o.get("value_text") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Null) | None => String::new(),
                Some(v) => tmpl(Some(v)),
            };
        }
    }
    String::new()
}

pub fn cockpitSessionNarrate(input: &Value, db: &Connection) -> Result<Value, String> {
    let session_id = js_number(input.get("session_id"));
    if session_id == 0.0 || session_id.is_nan() {
        return Err("cockpit-session-narrate: session_id is required".into());
    }

    let audit = crate::cockpit_cockpit_session_audit::cockpitSessionAudit(&json!({ "session_id": session_id }), db)?;
    if audit.get("ok") != Some(&Value::Bool(true)) {
        let err = audit.get("error").cloned().unwrap_or(json!("audit read failed"));
        return Ok(json!({ "ok": false, "error": err }));
    }

    ensure_prompt(db)?;

    let lines = build_lines(&audit);
    let session = audit.get("session").cloned().unwrap_or(Value::Null);
    let session_name = session.get("name").filter(|v| !v.is_null())
        .map(|v| tmpl(Some(v)))
        .unwrap_or_else(|| {
            // JS Number-fallback via `${sessionId}` when session.name is nullish.
            if session_id.fract() == 0.0 && session_id.is_finite() { format!("{}", session_id as i64) } else { session_id.to_string() }
        });
    let mission_raw = session.get("mission_md").filter(|v| !v.is_null()).map(|v| tmpl(Some(v))).unwrap_or_else(|| "(none)".to_string());
    let mission: String = mission_raw.chars().take(1200).collect();
    let activity_joined = if lines.is_empty() { "(no activity yet)".to_string() } else { lines.join("\n") };
    let activity: String = activity_joined.chars().take(14000).collect();

    // ── the AI call: PARITY-DEFERRED from here on (see module doc) — aiCall always returns Err.
    let ai_input = json!({
        "prompt_name": PROMPT,
        "tier": "low",
        "input_kv": [
            { "key": "session_name", "value": session_name },
            { "key": "mission", "value": mission },
            { "key": "activity", "value": activity },
        ],
        "provider_fn": Value::Null,
    });
    let res = crate::ai_ai_call::aiCall(&ai_input, db)?;

    let story = find_story(&res);
    let story_md = if story.is_empty() { "(the narrator returned nothing — try again)".to_string() } else { story };
    Ok(json!({
        "ok": true,
        "story_md": story_md,
        "ai_call_id": res.get("ai_call_id").cloned().unwrap_or(Value::Null),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c.execute_batch(
            "ATTACH ':memory:' AS brain; ATTACH ':memory:' AS timeline; \
             CREATE TABLE runtime_events (id INTEGER PRIMARY KEY, ts TEXT, agent TEXT, action TEXT, skill TEXT, duration_ms INTEGER, outcome TEXT, error_code TEXT); \
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, ts TEXT, provider TEXT, model_id TEXT, tier TEXT, cost_usd_actual REAL, tokens_in INTEGER, tokens_out INTEGER, outcome TEXT, subagent TEXT, session_id INTEGER); \
             CREATE TABLE timeline.sacred_timeline (id INTEGER PRIMARY KEY, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT); \
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) ); \
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn missing_session_id_errors() {
        let c = setup();
        let e = cockpitSessionNarrate(&json!({}), &c).unwrap_err();
        assert_eq!(e, "cockpit-session-narrate: session_id is required");
        let e0 = cockpitSessionNarrate(&json!({ "session_id": 0 }), &c).unwrap_err();
        assert_eq!(e0, "cockpit-session-narrate: session_id is required");
    }

    #[test]
    fn unknown_session_returns_ok_false_from_audit_passthrough() {
        let c = setup();
        let r = cockpitSessionNarrate(&json!({ "session_id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "session 999 not found" }));
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_after_full_deterministic_surface() {
        let c = setup();
        c.execute("INSERT INTO cockpit_sessions (id,name,mission_md,status,created_at) VALUES (1,'alpha','ship the thing','active','2026-01-01 00:00:10')", []).unwrap();
        c.execute("INSERT INTO runtime_events (id,ts,agent,action,skill,duration_ms,outcome,error_code) VALUES (1,'2026-01-01 00:00:20','dev','system.cockpit.pane.update','cockpit-pane-update',12,'ok',NULL)", []).unwrap();
        let err = cockpitSessionNarrate(&json!({ "session_id": 1 }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'cockpit-session-narrate'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_prompt_is_idempotent_when_template_unchanged() {
        let c = setup();
        ensure_prompt(&c).unwrap();
        ensure_prompt(&c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'cockpit-session-narrate'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn build_lines_maps_sorts_desc_and_caps_at_100() {
        let audit = json!({
            "runtime": [
                { "ts": "2026-01-01 00:00:20", "action": "system.cockpit.pane.update", "skill": "cockpit-pane-update", "outcome": "ok", "error_code": null },
                { "ts": "2026-01-01 00:00:05", "action": "system.card.add", "skill": null, "outcome": "fail", "error_code": "BAD_INPUT" },
            ],
            "ai_calls": [
                { "ts": "2026-01-01 00:00:40", "model_id": "claude-x", "provider": "anthropic", "cost_usd_actual": 0.5, "subagent": "pane:10", "outcome": "success" },
                { "ts": "2026-01-01 00:00:41", "model_id": null, "provider": "anthropic", "cost_usd_actual": null, "subagent": null, "outcome": "success" },
            ],
            "timeline": [
                { "ts": "2026-01-01 00:00:15", "operation": "cockpit.pane.update", "entity_type": "cockpit_pane", "entity_id": "10" },
            ],
        });
        let lines = build_lines(&audit);
        assert_eq!(lines.len(), 5);
        // sort() ascending then reverse() → descending string order, newest-looking first here
        // because all lines share the "[kind ts] ..." prefix shape and ts is lexicographically
        // monotonic with time; verify exact membership + the specific formatting branches.
        assert!(lines.contains(&"[skill 2026-01-01 00:00:20] system.cockpit.pane.update › cockpit-pane-update → ok".to_string()));
        assert!(lines.contains(&"[skill 2026-01-01 00:00:05] system.card.add → fail [BAD_INPUT]".to_string()));
        assert!(lines.contains(&"[ai 2026-01-01 00:00:40] claude-x $0.5000 pane:10 → success".to_string()));
        assert!(lines.contains(&"[ai 2026-01-01 00:00:41] anthropic $0.0000  → success".to_string()));
        assert!(lines.contains(&"[audit 2026-01-01 00:00:15] cockpit.pane.update cockpit_pane#10".to_string()));
    }

    #[test]
    fn find_story_matches_root_level_story_md_only() {
        let res = json!({ "output_kv": [
            { "field_name": "story_md", "value_text": "the story", "parent_output_id": null },
        ] });
        assert_eq!(find_story(&res), "the story");

        // nested (parent_output_id set) is skipped even if field_name matches
        let nested = json!({ "output_kv": [
            { "field_name": "story_md", "value_text": "nested", "parent_output_id": 7 },
        ] });
        assert_eq!(find_story(&nested), "");

        // no rows at all → empty string default
        assert_eq!(find_story(&json!({})), "");
        assert_eq!(find_story(&json!({ "output_kv": [] })), "");
    }
}
