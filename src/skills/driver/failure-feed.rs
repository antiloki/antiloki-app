//! failure-feed.rs — twin of skills/driver/failure-feed.mjs (db READ lane, cross-db ATTACH:
//! brain/judgments/memory). NAMES ARE CONTRACT: `failureFeed` (default). Aggregates failures from
//! driver ticks, build/lint gates, court judgments, QA verify, AI-call errors, notifications and
//! error_events into one ts-sorted, pattern-grouped feed. Frozen clock: 2026-01-01 00:00:00.
// QUALITY: TBD — 7 independent try-swallow sources feeding one sort+group+stats pipeline
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } }

// JSON.stringify(n): whole-valued finite numbers render without a trailing ".0".
fn js_num_json(n: f64) -> Value { if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e15 { json!(n as i64) } else { json!(n) } }

// clampHead(s, n): String(s ?? '').trim(), then slice(0,n)+'…' if over length.
fn clamp_head(v: Option<&Value>, n: usize) -> String {
    let t = str_qq(v).trim().to_string();
    if t.chars().count() > n { format!("{}\u{2026}", t.chars().take(n).collect::<String>()) } else { t }
}
// JS `a || b` for two arbitrary Values.
fn or_fallback(a: Value, b: Value) -> Value { if truthy(Some(&a)) { a } else { b } }
// String.slice(-n): last n chars (or whole string if shorter).
fn slice_tail(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n { s.to_string() } else { s.chars().skip(count - n).collect() }
}
// Mirrors each source's `try { db.prepare(...).all(...) } catch { /* absent */ }` — a query error
// (missing table / unattached db) yields zero rows for that source, not a hard failure.
fn safe_query(db: &Connection, sql: &str, params: &[Value]) -> Vec<Value> {
    crate::db_json::query_rows(db, sql, params).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
}

fn how_to_proceed_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"=== HOW TO PROCEED \(([\w-]+)\) ===").unwrap())
}
fn version_suffix_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"-v\d+$").unwrap())
}

// build_gate class+remedy: the appended "HOW TO PROCEED" marker wins; else re-classify the tail
// via the already-converted adviseOnFailure twin.
fn build_gate_advice(ev: &str) -> (String, String) {
    if let Some(caps) = how_to_proceed_re().captures(ev) {
        let m0 = caps.get(0).unwrap();
        let class = caps.get(1).unwrap().as_str().to_string();
        let tail = &ev[m0.start()..];
        let parts: Vec<&str> = tail.split("===").collect();
        let remedy = parts.get(2).map(|s| s.trim().to_string()).unwrap_or_default();
        (class, remedy)
    } else {
        let tail_text = Value::String(slice_tail(ev, 3000));
        let adv = crate::driver__helpers_failure_advice::adviseOnFailure(&tail_text);
        (
            adv.get("failure_class").and_then(|v| v.as_str()).unwrap_or("unclassified").to_string(),
            adv.get("remedy_md").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        )
    }
}

// reasonOf(ev): JSON.parse(ev ?? '{}')?.reason ?? null — invalid JSON or missing reason → null.
fn reason_of(ev: Option<&Value>) -> Value {
    let text = match ev { None | Some(Value::Null) => "{}".to_string(), Some(Value::String(s)) => s.clone(), Some(o) => o.to_string() };
    match serde_json::from_str::<Value>(&text) {
        Ok(parsed) => nz(parsed.get("reason")).cloned().unwrap_or(Value::Null),
        Err(_) => Value::Null,
    }
}

struct GroupAccum { pattern_key: Value, kind: Value, count: i64, first_ts: Value, last_ts: Value, sample_title: Value, cards: Vec<Value> }

pub fn failureFeed(input: &Value, db: &Connection) -> Result<Value, String> {
    let days_num = js_number(input.get("days"));
    let days: f64 = if days_num.is_finite() && days_num > 0.0 { days_num } else { 30.0 };
    let limit_num = js_number(input.get("limit"));
    let limit_f: f64 = if limit_num.is_finite() && limit_num > 0.0 { limit_num.min(2000.0) } else { 400.0 };
    let limit_i: i64 = limit_f as i64;
    let since = format!("datetime({}, '-{} days')", crate::clock::now_sql_inner(), days.floor() as i64);

    let mut rows: Vec<Value> = Vec::new();

    // tick failures — the driver stopped/errored/escalated on a stage
    for t in safe_query(db, &format!(
        "SELECT id, started_at, card_slug, decided_agent, decided_action, decided_reason, outcome, error_message \
         FROM brain.driver_ticks \
         WHERE (outcome IN ('failed','escalated','halt_requested') OR error_message IS NOT NULL) AND started_at >= {since} \
         ORDER BY id DESC LIMIT ?"), &[json!(limit_i)]) {
        let outcome_disp = nz(t.get("outcome")).cloned().unwrap_or(json!("error"));
        let action_disp_title = nz(t.get("decided_action")).cloned().unwrap_or(json!("(no action)"));
        let action_disp_pk = nz(t.get("decided_action")).cloned().unwrap_or(json!("none"));
        let severity = if t.get("outcome").and_then(|v| v.as_str()) == Some("escalated") { "error" } else { "warn" };
        let detail_src = nz(t.get("error_message")).or_else(|| nz(t.get("decided_reason")));
        let mut o = Map::new();
        o.insert("kind".into(), json!("tick"));
        o.insert("ts".into(), t.get("started_at").cloned().unwrap_or(Value::Null));
        o.insert("card_slug".into(), t.get("card_slug").cloned().unwrap_or(Value::Null));
        o.insert("ref_id".into(), t.get("id").cloned().unwrap_or(Value::Null));
        o.insert("severity".into(), json!(severity));
        o.insert("title".into(), json!(format!("{} \u{2014} {}", tmpl(Some(&outcome_disp)), tmpl(Some(&action_disp_title)))));
        o.insert("detail".into(), json!(clamp_head(detail_src, 1200)));
        o.insert("pattern_key".into(), json!(format!("tick:{}:{}", tmpl(Some(&outcome_disp)), tmpl(Some(&action_disp_pk)))));
        rows.push(Value::Object(o));
    }

    // build gate REDs — class from the appended HOW TO PROCEED marker, else re-classify the tail
    for g in safe_query(db, &format!(
        "SELECT id, contract_slug, evidence_md, ts FROM build_gate_runs \
         WHERE passed = 0 AND COALESCE(skipped,0) = 0 AND ts >= {since} \
         ORDER BY id DESC LIMIT ?"), &[json!(limit_i)]) {
        let ev = str_qq(g.get("evidence_md"));
        let (failure_class, remedy_md) = build_gate_advice(&ev);
        let mut o = Map::new();
        o.insert("kind".into(), json!("build_gate"));
        o.insert("ts".into(), g.get("ts").cloned().unwrap_or(Value::Null));
        o.insert("contract_slug".into(), g.get("contract_slug").cloned().unwrap_or(Value::Null));
        o.insert("ref_id".into(), g.get("id").cloned().unwrap_or(Value::Null));
        o.insert("severity".into(), json!("error"));
        o.insert("title".into(), json!(format!("build RED \u{2014} {}", failure_class)));
        let tail_val = Value::String(slice_tail(&ev, 1200));
        o.insert("detail".into(), json!(clamp_head(Some(&tail_val), 1200)));
        if !remedy_md.is_empty() { o.insert("remedy_md".into(), Value::String(remedy_md)); }
        o.insert("pattern_key".into(), json!(format!("build:{}", failure_class)));
        rows.push(Value::Object(o));
    }

    // LINT / structural gate REDs — the real deterministic findings, not the synthetic court verdict
    for g in safe_query(db, &format!(
        "SELECT id, contract_slug, mode, version, findings_json, ts FROM lint_gate_runs \
         WHERE passed = 0 AND ts >= {since} \
         ORDER BY id DESC LIMIT ?"), &[json!(limit_i)]) {
        let raw = nz(g.get("findings_json")).and_then(|v| v.as_str()).map(|s| s.to_string()).unwrap_or_else(|| "[]".to_string());
        let findings: Vec<Value> = serde_json::from_str::<Value>(&raw).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
        let errs: Vec<Value> = findings.iter().filter(|f| f.get("severity").and_then(|v| v.as_str()) == Some("error")).cloned().collect();
        let primary = nz(errs.first().and_then(|f| f.get("kind"))).or_else(|| nz(findings.first().and_then(|f| f.get("kind")))).cloned().unwrap_or(json!("unclassified"));
        let source_list: &Vec<Value> = if !errs.is_empty() { &errs } else { &findings };
        let lines: Vec<String> = source_list.iter().take(6).map(|f| {
            let file_disp = nz(f.get("file")).cloned().unwrap_or(json!("?"));
            let detail_arg = nz(f.get("detail")).or_else(|| nz(f.get("kind"))).cloned().unwrap_or(json!(""));
            format!("\u{b7} {}: {}", tmpl(Some(&file_disp)), clamp_head(Some(&detail_arg), 200))
        }).collect();
        let detail_joined = lines.join("\n");
        let why = findings.iter().find(|f| f.get("kind").and_then(|v| v.as_str()) == Some("why")).and_then(|f| f.get("detail")).cloned();
        let file_suffix = errs.first().and_then(|f| f.get("file")).filter(|v| truthy(Some(*v))).map(|v| format!(" ({})", tmpl(Some(v)))).unwrap_or_default();
        let mode_disp = nz(g.get("mode")).cloned().unwrap_or(json!("lint"));
        let mut o = Map::new();
        o.insert("kind".into(), json!("lint_gate"));
        o.insert("ts".into(), g.get("ts").cloned().unwrap_or(Value::Null));
        o.insert("contract_slug".into(), g.get("contract_slug").cloned().unwrap_or(Value::Null));
        o.insert("ref_id".into(), g.get("id").cloned().unwrap_or(Value::Null));
        o.insert("severity".into(), json!("error"));
        o.insert("title".into(), json!(format!("{} gate RED \u{2014} {}{}", tmpl(Some(&mode_disp)), tmpl(Some(&primary)), file_suffix)));
        o.insert("detail".into(), if detail_joined.is_empty() { json!("(no findings recorded)") } else { json!(detail_joined) });
        if let Some(w) = why { if truthy(Some(&w)) { o.insert("remedy_md".into(), w); } }
        o.insert("pattern_key".into(), json!(format!("lint:{}", tmpl(Some(&primary)))));
        rows.push(Value::Object(o));
    }

    // judgment fails — the AI COURT rejected the work; lint fails are counted once via lint_gate
    for j in safe_query(db, &format!(
        "SELECT judgment_id, card_slug, scope_ref, goal_line, updated_at FROM judgments.judgment_cases \
         WHERE verdict = 'fail' AND updated_at >= {since} \
         ORDER BY updated_at DESC LIMIT ?"), &[json!(limit_i)]) {
        let is_lint = str_qq(j.get("goal_line")).contains("lint gate RED");
        if is_lint { continue; }
        let items = safe_query(db, "SELECT statement FROM judgments.judgment_items WHERE judgment_id = ? AND side='prosecutor' AND judge_value=1 LIMIT 4", &[j.get("judgment_id").cloned().unwrap_or(Value::Null)]);
        let upheld = items.iter().map(|x| format!("\u{b7} {}", clamp_head(x.get("statement"), 200))).collect::<Vec<_>>().join("\n");
        let scope_str = str_qq(j.get("scope_ref"));
        let scope_trimmed = version_suffix_re().replace(&scope_str, "").to_string();
        let mut o = Map::new();
        o.insert("kind".into(), json!("judgment"));
        o.insert("ts".into(), j.get("updated_at").cloned().unwrap_or(Value::Null));
        o.insert("card_slug".into(), j.get("card_slug").cloned().unwrap_or(Value::Null));
        o.insert("ref_id".into(), j.get("judgment_id").cloned().unwrap_or(Value::Null));
        o.insert("severity".into(), json!("warn"));
        o.insert("title".into(), json!(format!("court FAIL \u{2014} {}", tmpl(j.get("scope_ref")))));
        o.insert("detail".into(), if !upheld.is_empty() { json!(upheld) } else { json!(clamp_head(j.get("goal_line"), 400)) });
        o.insert("pattern_key".into(), json!(format!("judgment:court:{}", scope_trimmed)));
        rows.push(Value::Object(o));
    }

    // QA VERIFY (acceptance) fails — grouped by the dominant per-AC reason
    for run in safe_query(db, &format!(
        "SELECT id, contract_slug, ts, acceptance_fail, all_passed FROM qa_runs \
         WHERE COALESCE(all_passed, 1) = 0 AND ts >= {since} \
         ORDER BY id DESC LIMIT ?"), &[json!(limit_i)]) {
        let items = safe_query(db, "SELECT ac_index, kind, evidence_md FROM qa_run_results WHERE qa_run_id = ? AND passed = 0 ORDER BY ac_index", &[run.get("id").cloned().unwrap_or(Value::Null)]);
        let primary = {
            let r0 = reason_of(items.first().and_then(|it| it.get("evidence_md")));
            nz(Some(&r0)).cloned().or_else(|| nz(items.first().and_then(|it| it.get("kind"))).cloned()).unwrap_or(json!("fail"))
        };
        let detail_lines: Vec<String> = items.iter().take(8).map(|it| {
            let ac_i = nz(it.get("ac_index")).and_then(|v| v.as_i64()).unwrap_or(0) + 1;
            let reason = reason_of(it.get("evidence_md"));
            let shown = or_fallback(reason, it.get("evidence_md").cloned().unwrap_or(Value::Null));
            format!("\u{b7} AC#{} ({}): {}", ac_i, tmpl(it.get("kind")), clamp_head(Some(&shown), 180))
        }).collect();
        let detail_joined = detail_lines.join("\n");
        let ac_count = nz(run.get("acceptance_fail")).cloned().unwrap_or(json!(items.len() as i64));
        let mut o = Map::new();
        o.insert("kind".into(), json!("qa"));
        o.insert("ts".into(), run.get("ts").cloned().unwrap_or(Value::Null));
        o.insert("contract_slug".into(), run.get("contract_slug").cloned().unwrap_or(Value::Null));
        o.insert("ref_id".into(), run.get("id").cloned().unwrap_or(Value::Null));
        o.insert("severity".into(), json!("error"));
        o.insert("title".into(), json!(format!("QA verify FAILED \u{2014} {} AC(s), {}", tmpl(Some(&ac_count)), tmpl(Some(&primary)))));
        o.insert("detail".into(), if detail_joined.is_empty() { json!("(no per-AC results recorded)") } else { json!(detail_joined) });
        o.insert("pattern_key".into(), json!(format!("qa:{}", tmpl(Some(&primary)))));
        rows.push(Value::Object(o));
    }

    // AI call errors — provider failovers + schema-validation failures
    for c in safe_query(db, &format!(
        "SELECT id, ts, model_id, outcome, outcome_class, session_id, contract_slug FROM brain.ai_calls \
         WHERE outcome != 'success' AND ts >= {since} \
         ORDER BY id DESC LIMIT ?"), &[json!(limit_i)]) {
        let class_disp = nz(c.get("outcome_class")).cloned().unwrap_or_else(|| c.get("outcome").cloned().unwrap_or(Value::Null));
        let model_disp_title = nz(c.get("model_id")).cloned().unwrap_or(json!("(no model)"));
        let model_disp_pk = nz(c.get("model_id")).cloned().unwrap_or(json!("none"));
        let session_disp = nz(c.get("session_id")).cloned().unwrap_or(json!("\u{2014}"));
        let mut o = Map::new();
        o.insert("kind".into(), json!("ai_call"));
        o.insert("ts".into(), c.get("ts").cloned().unwrap_or(Value::Null));
        o.insert("contract_slug".into(), c.get("contract_slug").cloned().unwrap_or(Value::Null));
        o.insert("ref_id".into(), c.get("id").cloned().unwrap_or(Value::Null));
        o.insert("severity".into(), json!("warn"));
        o.insert("title".into(), json!(format!("{} \u{2014} {}", tmpl(Some(&class_disp)), tmpl(Some(&model_disp_title)))));
        o.insert("detail".into(), json!(format!("session {}", tmpl(Some(&session_disp)))));
        o.insert("pattern_key".into(), json!(format!("ai:{}:{}", tmpl(Some(&class_disp)), tmpl(Some(&model_disp_pk)))));
        rows.push(Value::Object(o));
    }

    // warn/error notifications — the operator-facing escalations
    for n in safe_query(db, &format!(
        "SELECT id, ts, kind, severity, message, related_card_slug, dismissed_at FROM notifications \
         WHERE severity IN ('warn','error') AND ts >= {since} \
         ORDER BY id DESC LIMIT ?"), &[json!(limit_i)]) {
        let title = or_fallback(json!(clamp_head(n.get("message"), 160)), n.get("kind").cloned().unwrap_or(Value::Null));
        let mut o = Map::new();
        o.insert("kind".into(), json!("notification"));
        o.insert("ts".into(), n.get("ts").cloned().unwrap_or(Value::Null));
        o.insert("card_slug".into(), n.get("related_card_slug").cloned().unwrap_or(Value::Null));
        o.insert("ref_id".into(), n.get("id").cloned().unwrap_or(Value::Null));
        o.insert("severity".into(), n.get("severity").cloned().unwrap_or(Value::Null));
        o.insert("dismissed".into(), json!(truthy(n.get("dismissed_at"))));
        o.insert("title".into(), title);
        o.insert("detail".into(), json!(clamp_head(n.get("message"), 1200)));
        let kind_default = json!("unknown");
        o.insert("pattern_key".into(), json!(format!("notif:{}", tmpl(nz(n.get("kind")).or(Some(&kind_default))))));
        rows.push(Value::Object(o));
    }

    // error_events — the structured substrate error log (memory.sqlite), when populated
    for e in safe_query(db, &format!(
        "SELECT id, ts, pattern_key, skill, message, recovery_outcome FROM memory.error_events \
         WHERE ts >= {since} ORDER BY id DESC LIMIT ?"), &[json!(limit_i)]) {
        let recovered = truthy(e.get("recovery_outcome"));
        let skill_disp = nz(e.get("skill")).cloned().unwrap_or(json!("substrate"));
        let mut o = Map::new();
        o.insert("kind".into(), json!("error_event"));
        o.insert("ts".into(), e.get("ts").cloned().unwrap_or(Value::Null));
        o.insert("ref_id".into(), e.get("id").cloned().unwrap_or(Value::Null));
        o.insert("severity".into(), json!(if recovered { "warn" } else { "error" }));
        o.insert("recovered".into(), json!(recovered));
        o.insert("title".into(), json!(format!("{} \u{2014} {}", tmpl(Some(&skill_disp)), clamp_head(e.get("message"), 140))));
        o.insert("detail".into(), json!(clamp_head(e.get("message"), 1200)));
        let pattern_default = json!("unclassified");
        o.insert("pattern_key".into(), json!(format!("event:{}", tmpl(nz(e.get("pattern_key")).or(Some(&pattern_default))))));
        rows.push(Value::Object(o));
    }

    // most-recent-first, stable ties (matches Array.prototype.sort's guaranteed stability)
    rows.sort_by(|a, b| str_qq(b.get("ts")).cmp(&str_qq(a.get("ts"))));
    let limit_usize = limit_i.max(0) as usize;
    let trimmed: Vec<Value> = rows.into_iter().take(limit_usize).collect();

    // pattern groups — recurrence count + span per pattern_key, in first-seen order
    let mut group_order: Vec<String> = Vec::new();
    let mut groups_map: HashMap<String, GroupAccum> = HashMap::new();
    for r in &trimmed {
        let pk_val = r.get("pattern_key").cloned().unwrap_or(Value::Null);
        let pk_key = str_qq(Some(&pk_val));
        if !groups_map.contains_key(&pk_key) {
            group_order.push(pk_key.clone());
            groups_map.insert(pk_key.clone(), GroupAccum {
                pattern_key: pk_val,
                kind: r.get("kind").cloned().unwrap_or(Value::Null),
                count: 0,
                first_ts: r.get("ts").cloned().unwrap_or(Value::Null),
                last_ts: r.get("ts").cloned().unwrap_or(Value::Null),
                sample_title: r.get("title").cloned().unwrap_or(Value::Null),
                cards: Vec::new(),
            });
        }
        let entry = groups_map.get_mut(&pk_key).unwrap();
        entry.count += 1;
        let r_ts = str_qq(r.get("ts"));
        if r_ts < str_qq(Some(&entry.first_ts)) { entry.first_ts = r.get("ts").cloned().unwrap_or(Value::Null); }
        if r_ts > str_qq(Some(&entry.last_ts)) { entry.last_ts = r.get("ts").cloned().unwrap_or(Value::Null); }
        if let Some(cs) = r.get("card_slug") {
            if truthy(Some(cs)) && !entry.cards.contains(cs) { entry.cards.push(cs.clone()); }
        }
    }
    let mut groups: Vec<Value> = group_order.iter().map(|k| {
        let g = groups_map.remove(k).unwrap();
        let mut cards = g.cards; cards.truncate(6);
        let mut o = Map::new();
        o.insert("pattern_key".into(), g.pattern_key);
        o.insert("kind".into(), g.kind);
        o.insert("count".into(), json!(g.count));
        o.insert("first_ts".into(), g.first_ts);
        o.insert("last_ts".into(), g.last_ts);
        o.insert("sample_title".into(), g.sample_title);
        o.insert("cards".into(), json!(cards));
        Value::Object(o)
    }).collect();
    groups.sort_by(|a, b| {
        let ca = a.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
        let cb = b.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
        cb.cmp(&ca)
    });

    let mut by_kind = Map::new();
    for r in &trimmed {
        let k = r.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let cur = by_kind.get(&k).and_then(|v| v.as_i64()).unwrap_or(0);
        by_kind.insert(k, json!(cur + 1));
    }
    let mut by_day = Map::new();
    for r in &trimmed {
        let d: String = str_qq(r.get("ts")).chars().take(10).collect();
        if !d.is_empty() {
            let cur = by_day.get(&d).and_then(|v| v.as_i64()).unwrap_or(0);
            by_day.insert(d, json!(cur + 1));
        }
    }

    let mut stats = Map::new();
    stats.insert("total".into(), json!(trimmed.len() as i64));
    stats.insert("by_kind".into(), Value::Object(by_kind));
    stats.insert("by_day".into(), Value::Object(by_day));
    stats.insert("days".into(), js_num_json(days));

    let mut out = Map::new();
    out.insert("rows".into(), json!(trimmed));
    out.insert("groups".into(), json!(groups));
    out.insert("stats".into(), Value::Object(stats));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS memory;
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY, started_at TEXT, card_slug TEXT, decided_agent TEXT, decided_action TEXT, decided_reason TEXT, outcome TEXT, error_message TEXT);
             CREATE TABLE build_gate_runs (id INTEGER PRIMARY KEY, contract_slug TEXT, evidence_md TEXT, ts TEXT, passed INTEGER, skipped INTEGER);
             CREATE TABLE lint_gate_runs (id INTEGER PRIMARY KEY, contract_slug TEXT, mode TEXT, version TEXT, findings_json TEXT, ts TEXT, passed INTEGER);
             CREATE TABLE judgments.judgment_cases (judgment_id TEXT, card_slug TEXT, scope_ref TEXT, goal_line TEXT, updated_at TEXT, verdict TEXT);
             CREATE TABLE judgments.judgment_items (judgment_id TEXT, side TEXT, judge_value INTEGER, statement TEXT);
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY, contract_slug TEXT, ts TEXT, acceptance_fail INTEGER, all_passed INTEGER);
             CREATE TABLE qa_run_results (qa_run_id INTEGER, ac_index INTEGER, kind TEXT, evidence_md TEXT, passed INTEGER);
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, ts TEXT, model_id TEXT, outcome TEXT, outcome_class TEXT, session_id TEXT, contract_slug TEXT);
             CREATE TABLE notifications (id INTEGER PRIMARY KEY, ts TEXT, kind TEXT, severity TEXT, message TEXT, related_card_slug TEXT, dismissed_at TEXT);
             CREATE TABLE memory.error_events (id INTEGER PRIMARY KEY, ts TEXT, pattern_key TEXT, skill TEXT, message TEXT, recovery_outcome TEXT);"
        ).unwrap();
        c
    }

    #[test]
    fn aggregates_sorts_and_groups_failures() {
        let c = make_db();
        c.execute_batch(
            "INSERT INTO brain.driver_ticks (id, started_at, card_slug, decided_action, outcome, error_message, decided_reason) VALUES
               (1, '2025-12-30 10:00:00', 'CARD-A', 'dev.impl', 'escalated', 'boom now', NULL),
               (2, '2025-12-15 08:00:00', NULL, NULL, 'failed', NULL, 'timed out waiting for gate'),
               (3, '2025-11-01 00:00:00', 'CARD-OLD', 'dev.impl', 'failed', 'old boom', NULL);
             INSERT INTO build_gate_runs (id, contract_slug, evidence_md, ts, passed, skipped) VALUES
               (1, 'C-100', 'log...\n=== HOW TO PROCEED (native-build-dep) ===\nRun npm install node-sass@npm:sass', '2025-12-28 12:00:00', 0, 0),
               (2, 'C-101', 'Build failed: SyntaxError: Unexpected token', '2025-12-20 09:00:00', 0, 0),
               (3, 'C-102', 'noise', '2025-12-19 09:00:00', 0, 1),
               (4, 'C-103', 'all good', '2025-12-19 09:00:00', 1, 0);
             INSERT INTO lint_gate_runs (id, contract_slug, mode, findings_json, ts, passed) VALUES
               (1, 'C-200', 'structural', '[{\"kind\":\"parse-error\",\"severity\":\"error\",\"file\":\"a.rs\",\"detail\":\"unexpected token at line 5\"},{\"kind\":\"why\",\"detail\":\"Root cause: two default exports collided.\"}]', '2025-12-27 11:00:00', 0),
               (2, 'C-201', 'structural', '[{\"kind\":\"declared-3x\",\"severity\":\"warn\",\"file\":\"c.rs\",\"detail\":\"declared 3 times\"}]', '2025-12-26 11:00:00', 0),
               (3, 'C-202', 'structural', NULL, '2025-12-25 11:00:00', 0);
             INSERT INTO judgments.judgment_cases (judgment_id, card_slug, scope_ref, goal_line, updated_at, verdict) VALUES
               ('J-A', 'CARD-B', 'contract-LINT', 'structural lint gate RED — parse-error (a.rs)', '2025-12-24 10:00:00', 'fail'),
               ('J-B', 'CARD-C', 'contract-XYZ-v3', 'some goal text', '2025-12-23 10:00:00', 'fail'),
               ('J-C', 'CARD-D', 'contract-ABC', 'another long goal line describing the failing scope', '2025-12-22 10:00:00', 'fail');
             INSERT INTO judgments.judgment_items (judgment_id, side, judge_value, statement) VALUES
               ('J-B', 'prosecutor', 1, 'Claim 1 upheld'), ('J-B', 'prosecutor', 1, 'Claim 2 upheld'), ('J-B', 'defense', 1, 'ignored side');
             INSERT INTO qa_runs (id, contract_slug, ts, acceptance_fail, all_passed) VALUES
               (1, 'C-300', '2025-12-21 10:00:00', 2, 0),
               (2, 'C-301', '2025-12-19 10:00:00', NULL, 0),
               (3, 'C-302', '2025-12-18 10:00:00', 1, 0),
               (4, 'C-303', '2025-12-17 10:00:00', 0, 1);
             INSERT INTO qa_run_results (qa_run_id, ac_index, kind, evidence_md, passed) VALUES
               (1, 0, 'file-exists', '{\"reason\":\"missing\"}', 0),
               (1, 1, 'test', NULL, 0),
               (3, 0, 'timeout', 'not-json', 0);
             INSERT INTO brain.ai_calls (id, ts, model_id, outcome, outcome_class, session_id, contract_slug) VALUES
               (1, '2025-12-17 10:00:00', 'claude-x', 'error', 'validation_failed', 'sess-1', 'C-400'),
               (2, '2025-12-16 10:00:00', NULL, 'error', NULL, NULL, NULL),
               (3, '2025-12-16 09:00:00', 'claude-x', 'success', NULL, 'sess-2', NULL);
             INSERT INTO notifications (id, ts, kind, severity, message, related_card_slug, dismissed_at) VALUES
               (1, '2025-12-14 10:00:00', 'warn-kind', 'warn', 'Something failed badly', 'CARD-E', NULL),
               (2, '2025-12-13 10:00:00', 'silent-kind', 'error', NULL, NULL, '2025-12-20 10:00:00'),
               (3, '2025-12-13 09:00:00', 'info-kind', 'info', 'ignored', NULL, NULL);
             INSERT INTO memory.error_events (id, ts, pattern_key, skill, message, recovery_outcome) VALUES
               (1, '2025-12-12 10:00:00', 'timeout', 'dev-agent', 'transient timeout', 'retried-ok'),
               (2, '2025-12-11 10:00:00', NULL, NULL, 'unrecoverable crash', NULL);"
        ).unwrap();

        let r = failureFeed(&json!({}), &c).unwrap();
        let rows = r["rows"].as_array().unwrap();

        // the >30-day-old tick row is excluded by the frozen `since` filter
        assert!(!rows.iter().any(|x| x["ref_id"] == json!(3) && x["kind"] == json!("tick")));
        // the lint-tagged judgment row is skipped (counted once via lint_gate)
        assert!(!rows.iter().any(|x| x["kind"] == json!("judgment") && x["ref_id"] == json!("J-A")));
        // skipped / passing build_gate & qa rows never surface
        assert!(!rows.iter().any(|x| x["kind"] == json!("build_gate") && x["ref_id"] == json!(3)));
        assert!(!rows.iter().any(|x| x["kind"] == json!("build_gate") && x["ref_id"] == json!(4)));
        assert!(!rows.iter().any(|x| x["kind"] == json!("qa") && x["ref_id"] == json!(4)));
        // success ai_call and info notification never surface
        assert!(!rows.iter().any(|x| x["kind"] == json!("ai_call") && x["ref_id"] == json!(3)));
        assert!(!rows.iter().any(|x| x["kind"] == json!("notification") && x["ref_id"] == json!(3)));

        // sorted most-recent-first
        let tss: Vec<String> = rows.iter().map(|x| x["ts"].as_str().unwrap().to_string()).collect();
        let mut sorted = tss.clone(); sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(tss, sorted);
        assert_eq!(rows[0]["kind"], json!("tick")); // 2025-12-30 is the newest

        let tick_escalated = rows.iter().find(|x| x["kind"] == json!("tick") && x["ref_id"] == json!(1)).unwrap();
        assert_eq!(tick_escalated["severity"], json!("error"));
        assert_eq!(tick_escalated["title"], json!("escalated — dev.impl"));
        assert_eq!(tick_escalated["pattern_key"], json!("tick:escalated:dev.impl"));
        let tick_failed = rows.iter().find(|x| x["kind"] == json!("tick") && x["ref_id"] == json!(2)).unwrap();
        assert_eq!(tick_failed["title"], json!("failed — (no action)"));
        assert_eq!(tick_failed["pattern_key"], json!("tick:failed:none")); // distinct default from title
        assert_eq!(tick_failed["detail"], json!("timed out waiting for gate"));

        let bg_marker = rows.iter().find(|x| x["kind"] == json!("build_gate") && x["ref_id"] == json!(1)).unwrap();
        assert_eq!(bg_marker["pattern_key"], json!("build:native-build-dep"));
        assert_eq!(bg_marker["remedy_md"], json!("Run npm install node-sass@npm:sass"));
        let bg_advise = rows.iter().find(|x| x["kind"] == json!("build_gate") && x["ref_id"] == json!(2)).unwrap();
        assert_eq!(bg_advise["pattern_key"], json!("build:syntax-error"));

        let lint_errs = rows.iter().find(|x| x["kind"] == json!("lint_gate") && x["ref_id"] == json!(1)).unwrap();
        assert_eq!(lint_errs["pattern_key"], json!("lint:parse-error"));
        assert_eq!(lint_errs["title"], json!("structural gate RED — parse-error (a.rs)"));
        assert_eq!(lint_errs["remedy_md"], json!("Root cause: two default exports collided."));
        let lint_warn = rows.iter().find(|x| x["kind"] == json!("lint_gate") && x["ref_id"] == json!(2)).unwrap();
        assert_eq!(lint_warn["pattern_key"], json!("lint:declared-3x"));
        assert!(lint_warn.get("remedy_md").is_none());
        let lint_empty = rows.iter().find(|x| x["kind"] == json!("lint_gate") && x["ref_id"] == json!(3)).unwrap();
        assert_eq!(lint_empty["pattern_key"], json!("lint:unclassified"));
        assert_eq!(lint_empty["detail"], json!("(no findings recorded)"));

        let judgB = rows.iter().find(|x| x["kind"] == json!("judgment") && x["ref_id"] == json!("J-B")).unwrap();
        assert_eq!(judgB["pattern_key"], json!("judgment:court:contract-XYZ")); // -v3 stripped
        assert!(judgB["detail"].as_str().unwrap().contains("Claim 1 upheld"));
        let judgC = rows.iter().find(|x| x["kind"] == json!("judgment") && x["ref_id"] == json!("J-C")).unwrap();
        assert_eq!(judgC["pattern_key"], json!("judgment:court:contract-ABC"));
        assert_eq!(judgC["detail"], json!("another long goal line describing the failing scope"));

        let qa1 = rows.iter().find(|x| x["kind"] == json!("qa") && x["ref_id"] == json!(1)).unwrap();
        assert_eq!(qa1["pattern_key"], json!("qa:missing"));
        assert!(qa1["detail"].as_str().unwrap().contains("AC#1 (file-exists): missing"));
        let qa2 = rows.iter().find(|x| x["kind"] == json!("qa") && x["ref_id"] == json!(2)).unwrap();
        assert_eq!(qa2["pattern_key"], json!("qa:fail")); // no items, acceptance_fail null -> items.length 0
        assert_eq!(qa2["detail"], json!("(no per-AC results recorded)"));
        let qa3 = rows.iter().find(|x| x["kind"] == json!("qa") && x["ref_id"] == json!(3)).unwrap();
        assert_eq!(qa3["pattern_key"], json!("qa:timeout")); // invalid-JSON reason falls back to item.kind

        let ai1 = rows.iter().find(|x| x["kind"] == json!("ai_call") && x["ref_id"] == json!(1)).unwrap();
        assert_eq!(ai1["title"], json!("validation_failed — claude-x"));
        let ai2 = rows.iter().find(|x| x["kind"] == json!("ai_call") && x["ref_id"] == json!(2)).unwrap();
        assert_eq!(ai2["title"], json!("error — (no model)"));
        assert_eq!(ai2["pattern_key"], json!("ai:error:none")); // distinct default from title
        assert_eq!(ai2["detail"], json!("session \u{2014}"));

        let n1 = rows.iter().find(|x| x["kind"] == json!("notification") && x["ref_id"] == json!(1)).unwrap();
        assert_eq!(n1["title"], json!("Something failed badly"));
        assert_eq!(n1["dismissed"], json!(false));
        let n2 = rows.iter().find(|x| x["kind"] == json!("notification") && x["ref_id"] == json!(2)).unwrap();
        assert_eq!(n2["title"], json!("silent-kind")); // empty message -> falls back to kind
        assert_eq!(n2["dismissed"], json!(true));

        let e1 = rows.iter().find(|x| x["kind"] == json!("error_event") && x["ref_id"] == json!(1)).unwrap();
        assert_eq!(e1["severity"], json!("warn"));
        assert_eq!(e1["recovered"], json!(true));
        let e2 = rows.iter().find(|x| x["kind"] == json!("error_event") && x["ref_id"] == json!(2)).unwrap();
        assert_eq!(e2["severity"], json!("error"));
        assert_eq!(e2["title"], json!("substrate — unrecoverable crash"));

        // grouping: two ai_call rows share pattern? no — verify a real recurrence via qa reason
        let groups = r["groups"].as_array().unwrap();
        let g_qa_missing = groups.iter().find(|g| g["pattern_key"] == json!("qa:missing")).unwrap();
        assert_eq!(g_qa_missing["count"], json!(1));
        // groups sorted descending by count
        for w in groups.windows(2) {
            assert!(w[0]["count"].as_i64().unwrap() >= w[1]["count"].as_i64().unwrap());
        }

        assert_eq!(r["stats"]["days"], json!(30));
        assert_eq!(r["stats"]["total"], json!(rows.len() as i64));
        let by_kind = r["stats"]["by_kind"].as_object().unwrap();
        let total_by_kind: i64 = by_kind.values().map(|v| v.as_i64().unwrap()).sum();
        assert_eq!(total_by_kind, rows.len() as i64);
    }

    #[test]
    fn fractional_days_floor_and_limit_trims() {
        let c = make_db();
        c.execute_batch(
            "INSERT INTO brain.driver_ticks (id, started_at, decided_action, outcome, error_message) VALUES
               (1, '2025-12-30 10:00:00', 'dev.impl', 'failed', 'recent enough'),
               (2, '2025-12-29 10:00:00', 'dev.impl', 'failed', 'also recent'),
               (3, '2025-12-28 23:59:59', 'dev.impl', 'failed', 'just outside the 2-day floor');"
        ).unwrap();
        // days=2.5 -> Math.floor -> since = now - 2 days = 2025-12-30 00:00:00; only row 1 qualifies
        let r = failureFeed(&json!({ "days": 2.5, "limit": 3 }), &c).unwrap();
        assert_eq!(r["stats"]["days"], json!(2.5)); // fractional input preserved verbatim in output
        let rows = r["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["ref_id"], json!(1));

        // limit trims the final row count even when more rows exist
        c.execute_batch(
            "INSERT INTO brain.driver_ticks (id, started_at, decided_action, outcome, error_message) VALUES
               (4, '2025-12-31 10:00:00', 'dev.impl', 'failed', 'newest'),
               (5, '2025-12-31 09:00:00', 'dev.impl', 'failed', 'newer');"
        ).unwrap();
        let r2 = failureFeed(&json!({ "limit": 2 }), &c).unwrap();
        let rows2 = r2["rows"].as_array().unwrap();
        assert_eq!(rows2.len(), 2);
        assert_eq!(rows2[0]["ref_id"], json!(4));
        assert_eq!(rows2[1]["ref_id"], json!(5));
        assert_eq!(r2["stats"]["total"], json!(2));
    }
}
