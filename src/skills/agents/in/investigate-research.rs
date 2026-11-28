//! investigate-research.rs — twin of skills/agents/in/investigate-research.mjs (db READ lane,
//! ATTACHed `memory`, `laws`, `brain`). NAMES ARE CONTRACT: `investigateResearch` (default). Pure
//! deterministic fan-out across substrate surfaces (memory corrections, cards, actions, skills,
//! laws, sacred_timeline, brain.driver_ticks, runtime_events) — NO AI. Each source degrades to []
//! on its own error, exactly like the JS source's per-source try/catch.
//!
//! Cross-module: memory/action/skill/law lookups reuse the already-converted
//! `entities_catalog_find::catalogFind` twin. That twin's vector-search branch is
//! PARITY-DEFERRED, but it is never entered here: this skill never passes an explicit `model`,
//! and as long as fixtures leave `memory.catalog_embeddings` empty for the queried entity_types,
//! `useModel` always resolves falsy, keeping every call on the converted deterministic
//! substring-fallback path. For `entity_type: 'memory-correction'` that fallback path has no
//! branch in the JS if/else chain and always throws ("Cannot read properties of undefined
//! (reading 'map')") — caught here exactly as the JS source's local try/catch catches it,
//! yielding [] for the memory source every time.
//!
//! Frozen-clock lane: JS `Date.now()` and `performance`-style elapsed computations are pinned to
//! 2026-01-01T00:00:00.000Z (1767225600000 ms); `took_ms` is therefore always `0`.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

const DEFAULT_PER_SOURCE: i64 = 5;
const DEFAULT_BUDGET: i64 = 30;

// Standard helpers (playbook, copied verbatim — only the ones this twin needs).
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
// JS Math.round: nearest integer, ties toward +Infinity.
fn js_round(x: f64) -> f64 { if !x.is_finite() { return x; } let f = x.floor(); if x - f < 0.5 { f } else { f + 1.0 } }

// days since 1970-01-01 for a proleptic-Gregorian y/m/d (m in 1..=12) — Howard Hinnant's algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
// Parse 'YYYY-MM-DD[T| ]HH:MM:SS[.sss][Z]' as a UTC instant → epoch ms (mirrors Date.parse of an
// already-normalized 'T'+'Z' string). Malformed → None (JS NaN, where Number.isNaN is true).
fn parse_iso_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' { return None; }
    let num = |lo: usize, hi: usize| -> Option<i64> {
        let sub = s.get(lo..hi)?;
        if sub.bytes().all(|c| c.is_ascii_digit()) { sub.parse::<i64>().ok() } else { None }
    };
    let y = num(0, 4)?;
    let mo = num(5, 7)?;
    let d = num(8, 10)?;
    let (mut hh, mut mm, mut ss, mut ms) = (0i64, 0i64, 0i64, 0i64);
    if b.len() > 10 {
        if b.len() < 19 || (b[10] != b'T' && b[10] != b' ') || b[13] != b':' || b[16] != b':' { return None; }
        hh = num(11, 13)?;
        mm = num(14, 16)?;
        ss = num(17, 19)?;
        if b.len() > 19 && b[19] == b'.' {
            let mut f3: String = s[20..].chars().take_while(|c| c.is_ascii_digit()).take(3).collect();
            while f3.len() < 3 { f3.push('0'); }
            ms = f3.parse::<i64>().unwrap_or(0);
        }
    }
    Some((((days_from_civil(y, mo, d) * 24 + hh) * 60 + mm) * 60 + ss) * 1000 + ms)
}

// JS: function ageSeconds(ts) { if (!ts || typeof ts !== 'string') return null;
//   const t = Date.parse(ts.replace(' ', 'T') + (ts.endsWith('Z') ? '' : 'Z'));
//   if (Number.isNaN(t)) return null;
//   return Math.max(0, Math.round((Date.now() - t) / 1000)); }
fn age_seconds(ts: Option<&Value>) -> Value {
    let s = match ts { Some(Value::String(s)) if !s.is_empty() => s.as_str(), _ => return Value::Null };
    let replaced = s.replacen(' ', "T", 1);
    let candidate = if s.ends_with('Z') { replaced } else { format!("{replaced}Z") };
    let parsed_ms = match parse_iso_ms(&candidate) { Some(ms) => ms, None => return Value::Null };
    let diff = js_round((crate::clock::now_ms() - parsed_ms) as f64 / 1000.0);
    let clamped = if diff < 0.0 { 0i64 } else { diff as i64 };
    json!(clamped)
}

// JS: function safeSnippet(s, n = 240) { if (s == null) return '';
//   const str = typeof s === 'string' ? s : JSON.stringify(s);
//   return str.replace(/\s+/g, ' ').slice(0, n); }
fn collapse_and_slice(s: &str, n: usize) -> String {
    let re = regex::Regex::new(r"\s+").unwrap();
    let collapsed = re.replace_all(s, " ").to_string();
    collapsed.chars().take(n).collect()
}
fn safe_snippet(v: Option<&Value>, n: usize) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => collapse_and_slice(s, n),
        Some(other) => collapse_and_slice(&serde_json::to_string(other).unwrap_or_default(), n),
    }
}
fn safe_snippet_str(s: &str) -> String { collapse_and_slice(s, 240) }

// JS: `${title} — ${body_json ?? ''}` then safeSnippet(...) — used by both searchCards and lookupSlugs.
fn title_body_snippet(title: Option<&Value>, body_json: Option<&Value>) -> String {
    let combined = format!("{} — {}", tmpl(title), match nz(body_json) { Some(v) => tmpl(Some(v)), None => String::new() });
    safe_snippet_str(&combined)
}

// JS: /[A-Z][A-Z0-9-]{3,}/g, deduped via Set (insertion order), then .slice(0, 5).
fn extract_slugs(question: &str) -> Vec<String> {
    let re = regex::Regex::new(r"[A-Z][A-Z0-9-]{3,}").unwrap();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for m in re.find_iter(question) {
        let s = m.as_str().to_string();
        if seen.insert(s.clone()) {
            out.push(s);
            if out.len() >= 5 { break; }
        }
    }
    out
}

fn like_pattern(query: &str) -> String {
    let sliced: String = query.chars().take(80).collect();
    format!("%{}%", sliced.replace('%', ""))
}

fn search_memory(db: &Connection, query: &str, limit: i64) -> Vec<Value> {
    let input = json!({ "query": query, "entity_type": "memory-correction", "limit": limit });
    let rows = match crate::entities_catalog_find::catalogFind(&input, db) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    rows.as_array().cloned().unwrap_or_default().into_iter().take(limit.max(0) as usize).map(|r| {
        let entity = r.get("entity");
        let id = entity.and_then(|e| e.get("id"));
        let entity_name = r.get("entity_name");
        let ref_val = format!("mem-{}", tmpl(nz(id).or(nz(entity_name))));
        let guidance = entity.and_then(|e| e.get("guidance"));
        let title = entity.and_then(|e| e.get("title"));
        let mut o = Map::new();
        o.insert("source".into(), json!("memory"));
        o.insert("kind".into(), json!("correction"));
        o.insert("ref".into(), json!(ref_val));
        o.insert("snippet".into(), json!(safe_snippet(nz(guidance).or(nz(title)), 240)));
        o.insert("score".into(), nz(r.get("score")).cloned().unwrap_or(Value::Null));
        Value::Object(o)
    }).collect()
}

fn search_catalog(db: &Connection, query: &str, limit: i64, entity_type: &str, source: &str, ref_field: &str, desc_field: &str) -> Vec<Value> {
    let input = json!({ "query": query, "entity_type": entity_type, "limit": limit });
    let rows = match crate::entities_catalog_find::catalogFind(&input, db) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    rows.as_array().cloned().unwrap_or_default().into_iter().take(limit.max(0) as usize).map(|r| {
        let entity = r.get("entity");
        let ref_field_v = entity.and_then(|e| e.get(ref_field));
        let entity_name = r.get("entity_name");
        let ref_opt = nz(ref_field_v).or(nz(entity_name)).cloned();
        let desc = entity.and_then(|e| e.get(desc_field));
        let mut o = Map::new();
        o.insert("source".into(), json!(source));
        o.insert("kind".into(), json!("catalog"));
        if let Some(rv) = ref_opt { o.insert("ref".into(), rv); }
        o.insert("snippet".into(), json!(safe_snippet(desc, 240)));
        o.insert("score".into(), nz(r.get("score")).cloned().unwrap_or(Value::Null));
        Value::Object(o)
    }).collect()
}

fn search_actions(db: &Connection, query: &str, limit: i64) -> Vec<Value> { search_catalog(db, query, limit, "action", "action", "name", "description") }
fn search_skills(db: &Connection, query: &str, limit: i64) -> Vec<Value> { search_catalog(db, query, limit, "skill", "skill", "name", "description") }

fn search_laws(db: &Connection, query: &str, limit: i64) -> Vec<Value> {
    let input = json!({ "query": query, "entity_type": "law", "limit": limit });
    let rows = match crate::entities_catalog_find::catalogFind(&input, db) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    rows.as_array().cloned().unwrap_or_default().into_iter().take(limit.max(0) as usize).map(|r| {
        let entity = r.get("entity");
        let slug = entity.and_then(|e| e.get("slug"));
        let entity_name = r.get("entity_name");
        let ref_opt = nz(slug).or(nz(entity_name)).cloned();
        let body_md = entity.and_then(|e| e.get("body_md"));
        let title = entity.and_then(|e| e.get("title"));
        let mut o = Map::new();
        o.insert("source".into(), json!("law"));
        o.insert("kind".into(), json!("rule"));
        if let Some(rv) = ref_opt { o.insert("ref".into(), rv); }
        o.insert("snippet".into(), json!(safe_snippet(nz(body_md).or(nz(title)), 240)));
        o.insert("score".into(), nz(r.get("score")).cloned().unwrap_or(Value::Null));
        Value::Object(o)
    }).collect()
}

fn search_cards(db: &Connection, query: &str, limit: i64) -> Vec<Value> {
    let like = like_pattern(query);
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT slug, title, status, body_json, updated_at FROM cards WHERE title LIKE ? OR body_json LIKE ? ORDER BY CASE WHEN status='doing' THEN 0 WHEN status='backlog' THEN 1 ELSE 2 END, updated_at DESC LIMIT ?",
        &[json!(like.clone()), json!(like), json!(limit)],
    ) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    rows.as_array().cloned().unwrap_or_default().into_iter().map(|r| {
        let mut o = Map::new();
        o.insert("source".into(), json!("card"));
        o.insert("kind".into(), r.get("status").cloned().unwrap_or(Value::Null));
        o.insert("ref".into(), r.get("slug").cloned().unwrap_or(Value::Null));
        o.insert("snippet".into(), json!(title_body_snippet(r.get("title"), r.get("body_json"))));
        o.insert("score".into(), Value::Null);
        o.insert("age_seconds".into(), age_seconds(r.get("updated_at")));
        Value::Object(o)
    }).collect()
}

fn lookup_slugs(db: &Connection, slugs: &[String]) -> Vec<Value> {
    if slugs.is_empty() { return Vec::new(); }
    let mut out = Vec::new();
    for slug in slugs {
        let card_rows = match crate::db_json::query_rows(
            db,
            "SELECT slug, title, status, body_json, updated_at, parent_slug FROM cards WHERE slug = ?",
            &[json!(slug)],
        ) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let card = match card_rows.as_array().and_then(|a| a.first()) { Some(c) => c.clone(), None => continue };
        let card_slug = card.get("slug").cloned().unwrap_or(Value::Null);

        let mut o = Map::new();
        o.insert("source".into(), json!("card"));
        o.insert("kind".into(), json!(format!("{} (named-by-user)", tmpl(card.get("status")))));
        o.insert("ref".into(), card_slug.clone());
        o.insert("snippet".into(), json!(title_body_snippet(card.get("title"), card.get("body_json"))));
        o.insert("score".into(), json!(1));
        o.insert("age_seconds".into(), age_seconds(card.get("updated_at")));
        out.push(Value::Object(o));

        let children = match crate::db_json::query_rows(
            db,
            "SELECT slug, title, status, updated_at FROM cards WHERE parent_slug = ? ORDER BY slug",
            &[card_slug.clone()],
        ) {
            Ok(v) => v.as_array().cloned().unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        for c in children.into_iter().take(20) {
            let mut co = Map::new();
            co.insert("source".into(), json!("card"));
            co.insert("kind".into(), json!(format!("{} (child-of-{})", tmpl(c.get("status")), tmpl(Some(&card_slug)))));
            co.insert("ref".into(), c.get("slug").cloned().unwrap_or(Value::Null));
            co.insert("snippet".into(), json!(safe_snippet(c.get("title"), 240)));
            co.insert("score".into(), json!(0.9));
            co.insert("age_seconds".into(), age_seconds(c.get("updated_at")));
            out.push(Value::Object(co));
        }

        if let Some(parent_slug) = nz(card.get("parent_slug")) {
            let parent_rows = crate::db_json::query_rows(
                db,
                "SELECT slug, title, status, updated_at FROM cards WHERE slug = ?",
                &[parent_slug.clone()],
            ).unwrap_or_else(|_| json!([]));
            if let Some(parent) = parent_rows.as_array().and_then(|a| a.first()) {
                let mut po = Map::new();
                po.insert("source".into(), json!("card"));
                po.insert("kind".into(), json!(format!("{} (parent-of-{})", tmpl(parent.get("status")), tmpl(Some(&card_slug)))));
                po.insert("ref".into(), parent.get("slug").cloned().unwrap_or(Value::Null));
                po.insert("snippet".into(), json!(safe_snippet(parent.get("title"), 240)));
                po.insert("score".into(), json!(0.9));
                po.insert("age_seconds".into(), age_seconds(parent.get("updated_at")));
                out.push(Value::Object(po));
            }
        }
    }
    out
}

fn search_timeline(db: &Connection, query: &str, limit: i64) -> Vec<Value> {
    let like = like_pattern(query);
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT id, ts, operation, entity_type, entity_id, actor FROM sacred_timeline WHERE operation LIKE ? OR entity_type LIKE ? ORDER BY id DESC LIMIT ?",
        &[json!(like.clone()), json!(like), json!(limit)],
    ) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    rows.as_array().cloned().unwrap_or_default().into_iter().map(|r| {
        let mut o = Map::new();
        o.insert("source".into(), json!("timeline"));
        o.insert("kind".into(), r.get("operation").cloned().unwrap_or(Value::Null));
        o.insert("ref".into(), json!(format!("timeline-{}", tmpl(r.get("id")))));
        o.insert("snippet".into(), json!(format!(
            "{} {} {} {}:{}",
            tmpl(r.get("ts")), tmpl(r.get("actor")), tmpl(r.get("operation")), tmpl(r.get("entity_type")), tmpl(r.get("entity_id"))
        )));
        o.insert("score".into(), Value::Null);
        o.insert("age_seconds".into(), age_seconds(r.get("ts")));
        Value::Object(o)
    }).collect()
}

fn recent_driver_ticks(db: &Connection, limit: i64) -> Vec<Value> {
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT id, started_at, outcome, card_slug, decided_action, decided_reason, error_message FROM brain.driver_ticks ORDER BY id DESC LIMIT ?",
        &[json!(limit)],
    ) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    rows.as_array().cloned().unwrap_or_default().into_iter().map(|r| {
        let card = match nz(r.get("card_slug")) { Some(v) => tmpl(Some(v)), None => "-".to_string() };
        let action = match nz(r.get("decided_action")) { Some(v) => tmpl(Some(v)), None => "-".to_string() };
        let reason = match nz(r.get("decided_reason")) { Some(v) => tmpl(Some(v)), None => String::new() };
        let error_message = match nz(r.get("error_message")) { Some(v) => tmpl(Some(v)), None => String::new() };
        let combined = format!("{} {} card={card} action={action} {reason} {error_message}", tmpl(r.get("started_at")), tmpl(r.get("outcome")));
        let mut o = Map::new();
        o.insert("source".into(), json!("driver_tick"));
        o.insert("kind".into(), r.get("outcome").cloned().unwrap_or(Value::Null));
        o.insert("ref".into(), json!(format!("tick-{}", tmpl(r.get("id")))));
        o.insert("snippet".into(), json!(safe_snippet_str(&combined)));
        o.insert("score".into(), Value::Null);
        o.insert("age_seconds".into(), age_seconds(r.get("started_at")));
        Value::Object(o)
    }).collect()
}

fn recent_runtime_errors(db: &Connection, limit: i64) -> Vec<Value> {
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT id, ts, action, skill, outcome, error_code, actor FROM runtime_events WHERE outcome IN ('error', 'blocked') ORDER BY id DESC LIMIT ?",
        &[json!(limit)],
    ) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    rows.as_array().cloned().unwrap_or_default().into_iter().map(|r| {
        let action_or_skill = nz(r.get("action")).or_else(|| nz(r.get("skill")));
        let suffix = if truthy(r.get("error_code")) { format!(" \u{b7} {}", tmpl(r.get("error_code"))) } else { String::new() };
        let mut o = Map::new();
        o.insert("source".into(), json!("runtime_error"));
        o.insert("kind".into(), r.get("outcome").cloned().unwrap_or(Value::Null));
        o.insert("ref".into(), json!(format!("rt-{}", tmpl(r.get("id")))));
        o.insert("snippet".into(), json!(format!(
            "{} {} {} {}{suffix}",
            tmpl(r.get("ts")), tmpl(r.get("actor")), tmpl(action_or_skill), tmpl(r.get("outcome"))
        )));
        o.insert("score".into(), Value::Null);
        o.insert("age_seconds".into(), age_seconds(r.get("ts")));
        Value::Object(o)
    }).collect()
}

// JS Set dedupe key for `.ref`, including the `undefined` case (Set can hold `undefined` itself).
fn dedupe_key(f: &Value) -> String {
    match f.get("ref") {
        Some(v) => format!("v:{}", serde_json::to_string(v).unwrap_or_default()),
        None => "u:undefined".to_string(),
    }
}

pub fn investigateResearch(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    let question = match obj.and_then(|o| o.get("question")) {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Err("investigate-research: question is required".into()),
    };
    let per_source_raw = obj.and_then(|o| o.get("max_findings_per_source")).and_then(|v| v.as_i64()).unwrap_or(DEFAULT_PER_SOURCE);
    let per_source = per_source_raw.max(1).min(20);
    let budget = obj.and_then(|o| o.get("budget_findings")).and_then(|v| v.as_i64()).unwrap_or(DEFAULT_BUDGET);

    let slugs = extract_slugs(&question);

    let mem = search_memory(db, &question, per_source);
    let actions = search_actions(db, &question, per_source);
    let skills = search_skills(db, &question, per_source);
    let laws = search_laws(db, &question, per_source);
    let cards = search_cards(db, &question, per_source);
    let timeline = search_timeline(db, &question, per_source);
    let ticks = recent_driver_ticks(db, per_source.min(5));
    let rt_errors = recent_runtime_errors(db, per_source.min(5));
    let slug_hits = lookup_slugs(db, &slugs);

    let mut ordered: Vec<Value> = Vec::new();
    ordered.extend(slug_hits);
    ordered.extend(cards);
    ordered.extend(mem);
    ordered.extend(actions);
    ordered.extend(skills);
    ordered.extend(laws);
    ordered.extend(timeline);
    ordered.extend(ticks);
    ordered.extend(rt_errors);

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut deduped: Vec<Value> = Vec::new();
    for f in ordered {
        let key = dedupe_key(&f);
        if seen.contains(&key) { continue; }
        seen.insert(key);
        deduped.push(f);
    }

    let budget_clamped = budget.max(0);
    let findings: Vec<Value> = deduped.iter().take(budget_clamped as usize).cloned().collect();

    let mut by_source = Map::new();
    for f in &findings {
        let src = f.get("source").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let cur = by_source.get(&src).and_then(|v| v.as_i64()).unwrap_or(0);
        by_source.insert(src, json!(cur + 1));
    }

    let truncated = if (deduped.len() as i64) > budget_clamped { (deduped.len() as i64) - budget_clamped } else { 0 };

    Ok(json!({
        "findings": findings,
        "stats": {
            "by_source": by_source,
            "total": findings.len() as i64,
            "truncated": truncated,
            "took_ms": 0,
            "slugs_extracted": slugs,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory; ATTACH ':memory:' AS laws; ATTACH ':memory:' AS brain;
             CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, body_json TEXT, updated_at TEXT, parent_slug TEXT);
             CREATE TABLE actions (name TEXT, description TEXT);
             CREATE TABLE agents (name TEXT, description TEXT);
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, ts TEXT, operation TEXT, entity_type TEXT, entity_id TEXT, actor TEXT);
             CREATE TABLE runtime_events (id INTEGER PRIMARY KEY, ts TEXT, action TEXT, skill TEXT, outcome TEXT, error_code TEXT, actor TEXT);
             CREATE TABLE memory.catalog_embeddings (entity_type TEXT NOT NULL, entity_name TEXT NOT NULL, model TEXT NOT NULL, dims INTEGER NOT NULL, vector BLOB NOT NULL);
             CREATE TABLE memory.skills (name TEXT, description TEXT);
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, title TEXT);
             CREATE TABLE laws.laws (slug TEXT, title TEXT, statement TEXT, rationale TEXT, superseded_by INTEGER);
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY, started_at TEXT, outcome TEXT, card_slug TEXT, decided_action TEXT, decided_reason TEXT, error_message TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn requires_question() {
        let c = db();
        assert!(investigateResearch(&json!({}), &c).is_err());
    }

    #[test]
    fn fans_out_and_dedupes_direct_slug_hit() {
        let c = db();
        c.execute_batch(
            "INSERT INTO cards (slug, title, status, body_json, updated_at, parent_slug) VALUES
               ('EPIC-DIRECT-CARD', 'EPIC-DIRECT-CARD parent epic', 'doing', '{\"kind\":\"epic\"}', '2025-12-15 09:00:00', NULL),
               ('FEAT-DIRECT-CHILD', 'child feature under EPIC-DIRECT-CARD', 'backlog', '{}', '2025-12-18 09:00:00', 'EPIC-DIRECT-CARD');
             INSERT INTO actions (name, description) VALUES ('audit-log', 'writes an audit entry');",
        ).unwrap();
        let r = investigateResearch(&json!({ "question": "EPIC-DIRECT-CARD" }), &c).unwrap();
        let findings = r["findings"].as_array().unwrap();
        // slug-direct hit for EPIC-DIRECT-CARD wins over the LIKE-search card match (dedup by ref).
        let epic_hits: Vec<&Value> = findings.iter().filter(|f| f["ref"] == json!("EPIC-DIRECT-CARD")).collect();
        assert_eq!(epic_hits.len(), 1);
        assert_eq!(epic_hits[0]["kind"], json!("doing (named-by-user)"));
        assert!(findings.iter().any(|f| f["ref"] == json!("FEAT-DIRECT-CHILD") && f["kind"] == json!("backlog (child-of-EPIC-DIRECT-CARD)")));
        assert_eq!(r["stats"]["slugs_extracted"], json!(["EPIC-DIRECT-CARD"]));
    }

    #[test]
    fn runtime_error_suffix_and_action_or_skill_fallback() {
        let c = db();
        c.execute_batch(
            "INSERT INTO runtime_events (id, ts, action, skill, outcome, error_code, actor) VALUES
               (1, '2025-12-30 00:00:00', NULL, 'card-add', 'error', 'PERMISSION_DENIED', 'dev'),
               (2, '2025-12-31 00:00:00', 'card.update', NULL, 'blocked', NULL, 'qa');",
        ).unwrap();
        let r = investigateResearch(&json!({ "question": "nomatch" }), &c).unwrap();
        let findings = r["findings"].as_array().unwrap();
        let e1 = findings.iter().find(|f| f["ref"] == json!("rt-2")).unwrap();
        assert!(e1["snippet"].as_str().unwrap().contains("card.update blocked"));
        let e2 = findings.iter().find(|f| f["ref"] == json!("rt-1")).unwrap();
        assert!(e2["snippet"].as_str().unwrap().contains("card-add error \u{b7} PERMISSION_DENIED"));
    }

    #[test]
    fn budget_truncates_and_by_source_counts() {
        let c = db();
        c.execute_batch(
            "INSERT INTO cards (slug, title, status, body_json, updated_at, parent_slug) VALUES
               ('AUDIT-A', 'Audit trail A', 'doing', '{}', '2025-12-20 08:00:00', NULL),
               ('AUDIT-B', 'Audit trail B', 'backlog', '{}', '2025-11-01 08:00:00', NULL);",
        ).unwrap();
        let r = investigateResearch(&json!({ "question": "audit", "budget_findings": 1 }), &c).unwrap();
        assert_eq!(r["stats"]["total"], json!(1));
        assert_eq!(r["stats"]["truncated"], json!(1));
        assert_eq!(r["stats"]["took_ms"], json!(0));
    }
}
