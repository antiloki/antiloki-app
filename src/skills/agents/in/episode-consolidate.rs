//! episode-consolidate.rs — twin of skills/agents/in/episode-consolidate.mjs (db lane). NAMES ARE
//! CONTRACT: `episodeConsolidate` (default), `PROMPT_NAME`, `PROMPT_TEMPLATE_MD` (consts, exposed
//! as `pub const` below).
//!
//! ⛨ THE SENTINEL closes an episode. Two layers, per doctrine (deterministic first, AI once and
//! gated):
//!   1. DETERMINISTIC (always): the report — turns, cost, models, widgets by kind, artefacts,
//!      dispatches, the opening story, and FLAGS from printed thresholds. Pure arithmetic over the
//!      episode's own rows.
//!   2. AUTHORED (gated): ONE low-tier ai-call writes a headline + narrative, skipped for tiny
//!      episodes (< 3 turns) and on any AI failure — the deterministic report always lands.
//!
//! PARITY-DEFERRED boundary: `crate::ai_ai_call::aiCall` always returns `Err` here (see
//! ai-call.rs's own module doc — the provider dispatch is a real, non-deterministic network
//! round-trip, unconvertible in principle). Because the JS wraps that call in a try/catch that
//! swallows ALL failures ("deterministic report still lands"), this twin's AI branch always falls
//! through to the no-op path: `ai_call_id` stays `None`, `report.headline`/`narrative_md` are never
//! set, `cost_usd` stays 0. Everything BEFORE that call — the arithmetic report, `computeFlags`,
//! the `episode-meta` reason lookup, `ensurePrompt`'s find-or-register flow, and the `beats_md`
//! assembly — is ported faithfully and covered by the native tests below.
//!
//! Cross-module calls (pre-existing Rust twins): `crate::ai_prompt_find::promptFind`,
//! `crate::ai_prompt_add::promptAdd`, `crate::ai_ai_call::aiCall`,
//! `crate::agents_in_turn_widget_save::turnWidgetSave`.
//!
//! `context.db` missing → JS `err.contextMissing('episode-consolidate', 'db')`. Not modeled: `db:
//! &Connection` stays a required, non-nullable reference (every sibling db-lane twin's
//! convention).
// QUALITY: bespoke-native — deterministic report/flags/db-bookkeeping ported and tested; the
// aiCall network dispatch is the disclosed PARITY-DEFERRED boundary (inherited from ai-call.rs,
// not re-litigated here).

use rusqlite::Connection;
use serde_json::{json, Map, Value};

pub const PROMPT_NAME: &str = "sentinel.episode.v1";

pub const PROMPT_TEMPLATE_MD: &str = "You are THE SENTINEL of antiloki — the watcher that writes an episode's closing record. Write the account of ONE finished episode, grounded ONLY in the numbers and beats given below. NO INVENTION: never name files, people, costs, tools, or events the data does not contain.

THE DETERMINISTIC REPORT (arithmetic over the episode's rows — the ground truth):
{{report_json}}

THE LAST BEATS (operator ↔ assistant, oldest first, truncated):
{{beats_md}}

WHY THE EPISODE OPENED: {{reason}}

RULES:
1. headline: ≤ 60 characters, names the episode's real subject (take it from the beats), plain words, no fluff, no trailing period.
2. narrative_md: 2–4 sentences, past tense, factual — what was worked on, what was produced, what it cost. Quote numbers from the report verbatim (cost, counts). If the report carries flags, mention the most severe one in one clause.
3. Write like a ship's log: calm, precise, zero marketing. Never address the reader.

OUTPUT — return ONLY a JSON object of this exact shape (no prose, no markdown fences):
{
  \"headline\": \"<= 60 chars>\",
  \"narrative_md\": \"<2-4 sentences>\"
}";

// below this, an authored narrative is ceremony — the deterministic line already tells it all.
const MIN_TURNS_FOR_NARRATIVE: i64 = 3;

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
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
// JS Math.round: nearest integer, ties toward +Infinity.
fn js_round(x: f64) -> f64 {
    if !x.is_finite() { return x; }
    let f = x.floor();
    if x - f < 0.5 { f } else { f + 1.0 }
}
fn round4(x: f64) -> f64 { js_round(x * 10000.0) / 10000.0 }
// JS `String(v ?? '').replace(/\s+/g, ' ').trim()` — collapse any whitespace run to one space
// and drop leading/trailing whitespace in one pass (split_whitespace already discards the
// empty tokens a leading/trailing run would otherwise leave).
fn collapse_ws(v: Option<&Value>) -> String {
    let s = match v { None | Some(Value::Null) => String::new(), Some(x) => js_string(x) };
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
// `.slice(0, n)` on a JS string — approximated as a codepoint take (playbook convention; the
// episode text this feeds is operator-typed prose, not adversarial UTF-16 surrogate content).
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }

/// `ensurePrompt(db)` — find-or-register. Return value unused by the caller (JS `ensurePrompt(db);`
/// discards it too); only the registration side effect matters.
fn ensure_prompt(db: &Connection) {
    if crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db).is_ok() {
        return;
    }
    let _ = crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "low",
            "default_temperature": "cold",
            "max_output_tokens": 500,
            "output_fields": [
                { "field_name": "headline", "field_type": "string", "required": true },
                { "field_name": "narrative_md", "field_type": "string", "required": true },
            ],
        }),
        db,
    );
}

/// `computeFlags(report, costs)` — printed thresholds; the FE's live watcher
/// (sentinel-watch.ts) mirrors these exact numbers. Pure.
fn compute_flags(cost_usd: f64, turns: i64, widgets: i64, artifacts: i64, dispatched: i64, costs: &[f64]) -> Vec<Value> {
    let mut flags = Vec::new();
    if cost_usd >= 1.0 {
        flags.push(json!({ "sev": "warn", "text": format!("episode spend ${cost_usd} ≥ $1.00") }));
    } else if cost_usd >= 0.25 {
        flags.push(json!({ "sev": "notice", "text": format!("episode spend ${cost_usd} ≥ $0.25") }));
    }
    if costs.len() >= 4 {
        let mut sorted = costs.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let med = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
        let last = costs.last().copied().unwrap_or(0.0);
        if last >= (0.02_f64).max(3.0 * med) {
            let last_r = round4(last);
            flags.push(json!({ "sev": "notice", "text": format!("cost spike — the final turn (${last_r}) ran ≥ 3× the episode median") }));
        }
    }
    if turns >= 60 {
        flags.push(json!({ "sev": "notice", "text": format!("{turns} turns — a long chapter") }));
    }
    if turns >= 10 && widgets == 0 && artifacts == 0 && dispatched == 0 {
        flags.push(json!({ "sev": "notice", "text": "a long talking chapter — nothing produced, nothing dispatched" }));
    }
    flags
}

/// `export default async function episodeConsolidate(input, context)`
#[allow(non_snake_case)]
pub fn episodeConsolidate(input: &Value, db: &Connection) -> Result<Value, String> {
    // `Number(input?.session_id)`; NaN or non-positive-integer both fall to the same `required`
    // message (JS never distinguishes "wrong type" from "missing" here).
    let session_num = input.get("session_id").and_then(|v| match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok().or_else(|| if s.trim().is_empty() { Some(0.0) } else { None }),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Null => Some(0.0),
        _ => None,
    });
    let session_id = match session_num {
        Some(f) if f.is_finite() && f.fract() == 0.0 && f > 0.0 => f as i64,
        _ => return Err("episode-consolidate: session_id is required".into()),
    };

    let sess = crate::db_json::query_rows(db, "SELECT id FROM interviewer_sessions WHERE id = ?", &[json!(session_id)])?;
    if sess.as_array().map(|a| a.is_empty()).unwrap_or(true) {
        return Err(format!("episode-consolidate: session '{session_id}' not found"));
    }

    let turns = crate::db_json::query_rows(
        db,
        "SELECT ts, user_text, interviewer_response_md, ai_cost_usd, model_id, dispatched_action_ids_json \
         FROM interviewer_turns WHERE session_id = ? ORDER BY turn_index ASC",
        &[json!(session_id)],
    )?;
    let turns: Vec<Value> = turns.as_array().cloned().unwrap_or_default();
    let widget_rows = crate::db_json::query_rows(
        db,
        "SELECT type, kind FROM interviewer_session_widgets WHERE session_id = ?",
        &[json!(session_id)],
    )?;
    let widget_rows: Vec<Value> = widget_rows.as_array().cloned().unwrap_or_default();

    // ── layer 1: the arithmetic
    let mut cost = 0.0_f64;
    let mut dispatched = 0_i64;
    let mut models_order: Vec<String> = Vec::new();
    let mut models_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut costs: Vec<f64> = Vec::new();
    for t in &turns {
        let c = t.get("ai_cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
        cost += c;
        costs.push(c);
        let model_raw = t.get("model_id").and_then(|v| if v.is_null() { None } else { Some(js_string(v)) }).unwrap_or_default();
        if let Some(m) = model_raw.split('/').last() {
            if !m.is_empty() && models_seen.insert(m.to_string()) { models_order.push(m.to_string()); }
        }
        if let Some(Value::String(s)) = t.get("dispatched_action_ids_json") {
            if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(s) {
                if !a.is_empty() { dispatched += 1; }
            }
        }
    }
    let mut kinds: Vec<(String, i64)> = Vec::new();
    let mut widgets = 0_i64;
    let mut artifacts = 0_i64;
    for w in &widget_rows {
        let kind = w.get("kind").and_then(Value::as_str).unwrap_or("");
        if kind == "session-context" || kind == "episode-meta" || kind == "episode-report" { continue; }
        if w.get("type").and_then(Value::as_str) == Some("artifact") { artifacts += 1; continue; }
        widgets += 1;
        match kinds.iter_mut().find(|(k, _)| k == kind) {
            Some(entry) => entry.1 += 1,
            None => kinds.push((kind.to_string(), 1)),
        }
    }
    let mut sorted_kinds = kinds.clone();
    sorted_kinds.sort_by(|a, b| b.1.cmp(&a.1)); // stable: ties keep insertion order
    let widget_kinds: Vec<Value> = sorted_kinds
        .into_iter()
        .take(3)
        .map(|(k, n)| json!(if n > 1 { format!("{k} ×{n}") } else { k }))
        .collect();

    let first = turns.iter().find(|t| !collapse_ws(t.get("user_text")).is_empty());
    let turns_len = turns.len() as i64;
    let cost_round = round4(cost);
    let mut report = Map::new();
    report.insert("turns".into(), json!(turns_len));
    report.insert("cost_usd".into(), json!(cost_round));
    report.insert("models".into(), json!(models_order.into_iter().take(3).collect::<Vec<_>>()));
    report.insert("widgets".into(), json!(widgets));
    report.insert("widget_kinds".into(), json!(widget_kinds));
    report.insert("artifacts".into(), json!(artifacts));
    report.insert("dispatched".into(), json!(dispatched));
    report.insert(
        "story".into(),
        match first {
            Some(t) => json!(take_chars(&collapse_ws(t.get("user_text")), 96)),
            None => Value::Null,
        },
    );
    report.insert("authored_by".into(), json!("sentinel.v1"));
    let flags = compute_flags(cost_round, turns_len, widgets, artifacts, dispatched, &costs);
    report.insert("flags".into(), json!(flags));

    // why the episode opened — grounds the narrative when the boundary wrote it
    let reason: Option<String> = crate::db_json::query_rows(
        db,
        "SELECT payload_json FROM interviewer_session_widgets \
         WHERE session_id = ? AND kind = 'episode-meta' ORDER BY id DESC LIMIT 1",
        &[json!(session_id)],
    )
    .ok()
    .and_then(|rows| rows.as_array().and_then(|a| a.first()).cloned())
    .and_then(|row| row.get("payload_json").and_then(Value::as_str).map(str::to_string))
    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    .and_then(|v| v.get("reason").and_then(Value::as_str).map(str::to_string));

    // ── layer 2: the authored account (one gated low-tier call; failure never blocks the report)
    let mut ai_call_id: Option<Value> = None;
    let mut ai_cost = 0.0_f64;
    if turns_len >= MIN_TURNS_FOR_NARRATIVE {
        ensure_prompt(db);
        let start = turns.len().saturating_sub(10);
        let beats_md = turns[start..]
            .iter()
            .filter_map(|t| {
                let q = take_chars(&collapse_ws(t.get("user_text")), 160);
                if q.is_empty() { return None; }
                let a = take_chars(&collapse_ws(t.get("interviewer_response_md")), 200);
                Some(if a.is_empty() { format!("- you: {q}") } else { format!("- you: {q} → assistant: {a}") })
            })
            .collect::<Vec<_>>()
            .join("\n");

        let ai_input = json!({
            "prompt_name": PROMPT_NAME,
            "input_kv": [
                { "key": "report_json", "value": serde_json::to_string(&Value::Object(report.clone())).unwrap_or_default() },
                { "key": "beats_md", "value": if beats_md.is_empty() { "(no beats)".to_string() } else { beats_md } },
                { "key": "reason", "value": reason.clone().unwrap_or_else(|| "(unknown — a pre-engine chapter)".to_string()) },
            ],
            "intent": "system.episode.consolidate",
        });

        // PARITY-DEFERRED boundary: real `aiCall` always Errs here (see module doc + ai-call.rs).
        // JS wraps this in try/catch and swallows ALL failures — "deterministic report still lands".
        if let Ok(ai_result) = crate::ai_ai_call::aiCall(&ai_input, db) {
            ai_call_id = nz(ai_result.get("ai_call_id")).cloned();
            for o in ai_result.get("output_kv").and_then(Value::as_array).into_iter().flatten() {
                if let Some(v) = o.get("value_text").and_then(Value::as_str) {
                    match o.get("field_name").and_then(Value::as_str) {
                        Some("headline") => { report.insert("headline".into(), json!(take_chars(v, 80))); }
                        Some("narrative_md") => { report.insert("narrative_md".into(), json!(take_chars(v, 1200))); }
                        _ => {}
                    }
                }
            }
        }
        if let Some(id) = ai_call_id.clone() {
            if let Ok(rows) = crate::db_json::query_rows(db, "SELECT cost_usd_actual FROM brain.ai_calls WHERE id = ?", &[id]) {
                ai_cost = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("cost_usd_actual")).and_then(Value::as_f64).unwrap_or(0.0);
            }
            report.insert("sentinel_ai_call_id".into(), ai_call_id.clone().unwrap_or(Value::Null));
            report.insert("sentinel_cost_usd".into(), json!(round4(ai_cost)));
        }
    }

    let report_value = Value::Object(report);
    let save_input = json!({
        "session_id": session_id,
        "turn_id": Value::Null,
        "type": "widget",
        "kind": "episode-report",
        "payload": report_value,
    });
    let saved = crate::agents_in_turn_widget_save::turnWidgetSave(&save_input, db)?;
    let row_id = nz(saved.get("id")).cloned().unwrap_or(Value::Null);

    Ok(json!({
        "ok": true,
        "report": report_value,
        "row_id": row_id,
        "ai_call_id": ai_call_id.unwrap_or(Value::Null),
        "cost_usd": round4(ai_cost),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; \
             CREATE TABLE interviewer_sessions (id INTEGER PRIMARY KEY); \
             CREATE TABLE interviewer_turns (id INTEGER PRIMARY KEY, session_id INTEGER, turn_index INTEGER, \
               ts TEXT, user_text TEXT, interviewer_response_md TEXT, ai_cost_usd REAL, model_id TEXT, \
               dispatched_action_ids_json TEXT); \
             CREATE TABLE interviewer_session_widgets (id INTEGER PRIMARY KEY, session_id INTEGER, turn_id INTEGER, \
               type TEXT, kind TEXT, payload_json TEXT); \
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) ); \
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, cost_usd_actual REAL);",
        )
        .unwrap();
        c
    }

    fn insert_turn(c: &Connection, session_id: i64, idx: i64, user_text: &str, resp: &str, cost: f64, model: &str, dispatched_json: Option<&str>) {
        c.execute(
            "INSERT INTO interviewer_turns (session_id, turn_index, ts, user_text, interviewer_response_md, ai_cost_usd, model_id, dispatched_action_ids_json) \
             VALUES (?1,?2,'2026-01-01T00:00:00Z',?3,?4,?5,?6,?7)",
            rusqlite::params![session_id, idx, user_text, resp, cost, model, dispatched_json],
        )
        .unwrap();
    }

    fn insert_widget(c: &Connection, session_id: i64, typ: &str, kind: &str, payload: &str) {
        c.execute(
            "INSERT INTO interviewer_session_widgets (session_id, turn_id, type, kind, payload_json) VALUES (?1, NULL, ?2, ?3, ?4)",
            rusqlite::params![session_id, typ, kind, payload],
        )
        .unwrap();
    }

    // ── guards ───────────────────────────────────────────────────────
    #[test]
    fn missing_session_id_returns_exact_message() {
        let c = db();
        assert_eq!(episodeConsolidate(&json!({}), &c).unwrap_err(), "episode-consolidate: session_id is required");
    }

    #[test]
    fn zero_or_negative_session_id_returns_required_message() {
        let c = db();
        assert_eq!(episodeConsolidate(&json!({"session_id": 0}), &c).unwrap_err(), "episode-consolidate: session_id is required");
        assert_eq!(episodeConsolidate(&json!({"session_id": -3}), &c).unwrap_err(), "episode-consolidate: session_id is required");
    }

    #[test]
    fn session_not_found_returns_exact_message() {
        let c = db();
        assert_eq!(episodeConsolidate(&json!({"session_id": 999}), &c).unwrap_err(), "episode-consolidate: session '999' not found");
    }

    // ── deterministic report, tiny episode (no narrative attempt) ─────
    #[test]
    fn tiny_episode_report_and_flags_and_no_ai_call() {
        let c = db();
        c.execute("INSERT INTO interviewer_sessions (id) VALUES (1)", []).unwrap();
        insert_turn(&c, 1, 0, "hello there", "hi", 0.001, "anthropic/claude-sonnet", None);
        let out = episodeConsolidate(&json!({"session_id": 1}), &c).unwrap();
        assert_eq!(out["ok"], json!(true));
        let report = &out["report"];
        assert_eq!(report["turns"], json!(1));
        assert_eq!(report["cost_usd"], json!(0.001));
        assert_eq!(report["models"], json!(["claude-sonnet"]));
        assert_eq!(report["widgets"], json!(0));
        assert_eq!(report["artifacts"], json!(0));
        assert_eq!(report["dispatched"], json!(0));
        assert_eq!(report["story"], json!("hello there"));
        assert_eq!(report["authored_by"], json!("sentinel.v1"));
        assert_eq!(report["flags"], json!([]));
        assert!(report.get("headline").is_none());
        assert_eq!(out["ai_call_id"], Value::Null);
        assert_eq!(out["cost_usd"], json!(0.0));
        assert!(out["row_id"].is_i64() || out["row_id"].is_u64());
    }

    // ── flags: cost thresholds ──────────────────────────────────────
    #[test]
    fn cost_flag_notice_at_quarter_dollar() {
        let c = db();
        c.execute("INSERT INTO interviewer_sessions (id) VALUES (1)", []).unwrap();
        insert_turn(&c, 1, 0, "x", "y", 0.3, "m", None);
        let out = episodeConsolidate(&json!({"session_id": 1}), &c).unwrap();
        let flags = out["report"]["flags"].as_array().unwrap();
        assert!(flags.iter().any(|f| f["sev"] == "notice" && f["text"].as_str().unwrap().contains("≥ $0.25")));
    }

    #[test]
    fn cost_flag_warn_at_one_dollar() {
        let c = db();
        c.execute("INSERT INTO interviewer_sessions (id) VALUES (1)", []).unwrap();
        insert_turn(&c, 1, 0, "x", "y", 1.5, "m", None);
        let out = episodeConsolidate(&json!({"session_id": 1}), &c).unwrap();
        let flags = out["report"]["flags"].as_array().unwrap();
        assert!(flags.iter().any(|f| f["sev"] == "warn" && f["text"].as_str().unwrap().contains("≥ $1.00")));
    }

    #[test]
    fn cost_spike_flag_needs_four_turns_and_3x_median() {
        let c = db();
        c.execute("INSERT INTO interviewer_sessions (id) VALUES (1)", []).unwrap();
        for (i, cost) in [0.001, 0.001, 0.001, 0.1].iter().enumerate() {
            insert_turn(&c, 1, i as i64, "x", "y", *cost, "m", None);
        }
        let out = episodeConsolidate(&json!({"session_id": 1}), &c).unwrap();
        let flags = out["report"]["flags"].as_array().unwrap();
        assert!(flags.iter().any(|f| f["text"].as_str().unwrap().contains("cost spike")));
    }

    #[test]
    fn long_chapter_flag_at_60_turns() {
        let c = db();
        c.execute("INSERT INTO interviewer_sessions (id) VALUES (1)", []).unwrap();
        for i in 0..60 { insert_turn(&c, 1, i, "x", "y", 0.0, "m", None); }
        let out = episodeConsolidate(&json!({"session_id": 1}), &c).unwrap();
        let flags = out["report"]["flags"].as_array().unwrap();
        assert!(flags.iter().any(|f| f["text"].as_str().unwrap().contains("long chapter")));
    }

    #[test]
    fn nothing_produced_flag() {
        let c = db();
        c.execute("INSERT INTO interviewer_sessions (id) VALUES (1)", []).unwrap();
        for i in 0..10 { insert_turn(&c, 1, i, "x", "y", 0.0, "m", None); }
        let out = episodeConsolidate(&json!({"session_id": 1}), &c).unwrap();
        let flags = out["report"]["flags"].as_array().unwrap();
        assert!(flags.iter().any(|f| f["text"].as_str().unwrap().contains("nothing produced")));
    }

    // ── widgets/kinds/artifacts bookkeeping ────────────────────────────
    #[test]
    fn widget_kinds_ranked_desc_and_meta_rows_excluded() {
        let c = db();
        c.execute("INSERT INTO interviewer_sessions (id) VALUES (1)", []).unwrap();
        insert_turn(&c, 1, 0, "x", "y", 0.0, "m", None);
        insert_widget(&c, 1, "widget", "note", "{}");
        insert_widget(&c, 1, "widget", "note", "{}");
        insert_widget(&c, 1, "widget", "plan", "{}");
        insert_widget(&c, 1, "artifact", "html", "{}");
        insert_widget(&c, 1, "widget", "session-context", "{}");
        insert_widget(&c, 1, "widget", "episode-meta", "{\"reason\":\"boundary\"}");
        let out = episodeConsolidate(&json!({"session_id": 1}), &c).unwrap();
        let report = &out["report"];
        assert_eq!(report["widgets"], json!(3));
        assert_eq!(report["artifacts"], json!(1));
        assert_eq!(report["widget_kinds"], json!(["note ×2", "plan"]));
    }

    #[test]
    fn dispatched_counts_only_nonempty_arrays() {
        let c = db();
        c.execute("INSERT INTO interviewer_sessions (id) VALUES (1)", []).unwrap();
        insert_turn(&c, 1, 0, "x", "y", 0.0, "m", Some("[1,2]"));
        insert_turn(&c, 1, 1, "x", "y", 0.0, "m", Some("[]"));
        insert_turn(&c, 1, 2, "x", "y", 0.0, "m", None);
        let out = episodeConsolidate(&json!({"session_id": 1}), &c).unwrap();
        assert_eq!(out["report"]["dispatched"], json!(1));
    }

    // ── narrative gate: AI branch reached (and always short-circuits) at >= 3 turns ────
    #[test]
    fn narrative_gate_reaches_deferred_ai_call_and_swallows_its_error() {
        let c = db();
        c.execute("INSERT INTO interviewer_sessions (id) VALUES (1)", []).unwrap();
        for i in 0..3 { insert_turn(&c, 1, i, "hi", "hello", 0.0, "m", None); }
        let out = episodeConsolidate(&json!({"session_id": 1}), &c).unwrap();
        // Deterministic report still lands; the deferred aiCall Err was swallowed, so no headline.
        assert!(out["report"].get("headline").is_none());
        assert_eq!(out["ai_call_id"], Value::Null);
        // ensure_prompt's find-or-register side effect ran.
        assert!(crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), &c).is_ok());
    }

    #[test]
    fn below_narrative_threshold_never_registers_prompt() {
        let c = db();
        c.execute("INSERT INTO interviewer_sessions (id) VALUES (1)", []).unwrap();
        insert_turn(&c, 1, 0, "hi", "hello", 0.0, "m", None);
        insert_turn(&c, 1, 1, "hi", "hello", 0.0, "m", None);
        let out = episodeConsolidate(&json!({"session_id": 1}), &c).unwrap();
        assert!(out["report"].get("headline").is_none());
        assert!(crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), &c).is_err());
    }

    // ── story: first turn with a non-blank (post-trim) user_text ──────
    #[test]
    fn story_skips_blank_turns_and_collapses_whitespace() {
        let c = db();
        c.execute("INSERT INTO interviewer_sessions (id) VALUES (1)", []).unwrap();
        insert_turn(&c, 1, 0, "   ", "y", 0.0, "m", None);
        insert_turn(&c, 1, 1, "hello   world\nfoo", "y", 0.0, "m", None);
        let out = episodeConsolidate(&json!({"session_id": 1}), &c).unwrap();
        assert_eq!(out["report"]["story"], json!("hello world foo"));
    }

    #[test]
    fn story_null_when_no_nonblank_turn() {
        let c = db();
        c.execute("INSERT INTO interviewer_sessions (id) VALUES (1)", []).unwrap();
        insert_turn(&c, 1, 0, "   ", "y", 0.0, "m", None);
        let out = episodeConsolidate(&json!({"session_id": 1}), &c).unwrap();
        assert_eq!(out["report"]["story"], Value::Null);
    }

    // ── models: dedup + insertion order + max 3 ────────────────────────
    #[test]
    fn models_are_deduped_by_last_path_segment_insertion_order_max_3() {
        let c = db();
        c.execute("INSERT INTO interviewer_sessions (id) VALUES (1)", []).unwrap();
        insert_turn(&c, 1, 0, "x", "y", 0.0, "anthropic/sonnet", None);
        insert_turn(&c, 1, 1, "x", "y", 0.0, "openai/gpt", None);
        insert_turn(&c, 1, 2, "x", "y", 0.0, "anthropic/sonnet", None);
        insert_turn(&c, 1, 3, "x", "y", 0.0, "google/gemini", None);
        insert_turn(&c, 1, 4, "x", "y", 0.0, "extra/model4", None);
        let out = episodeConsolidate(&json!({"session_id": 1}), &c).unwrap();
        assert_eq!(out["report"]["models"], json!(["sonnet", "gpt", "gemini"]));
    }

    // ── row_id: the saved episode-report row is persisted via turn-widget-save ─────────
    #[test]
    fn persists_episode_report_row_via_turn_widget_save() {
        let c = db();
        c.execute("INSERT INTO interviewer_sessions (id) VALUES (1)", []).unwrap();
        insert_turn(&c, 1, 0, "x", "y", 0.0, "m", None);
        let out = episodeConsolidate(&json!({"session_id": 1}), &c).unwrap();
        let row_id = out["row_id"].as_i64().unwrap();
        let saved: String = c.query_row("SELECT kind FROM interviewer_session_widgets WHERE id = ?", [row_id], |r| r.get(0)).unwrap();
        assert_eq!(saved, "episode-report");
    }
}
