//! interviewer-pl-clarify-floor.rs — twin of skills/agents/in/interviewer-pl-clarify-floor.mjs
//! (standard default + helpers). NAMES ARE CONTRACT: `interviewerPlClarifyFloor` (default) ·
//! extractSlugs · hasPendingTrigger · queryPending (db-first) · groupByCard · formatResponse.
// QUALITY: 82/100 — deterministic floor: surface pending PL clarify Qs on slug-mention / status-trigger; defer on start-work
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
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
fn tmpl(v: Option<&Value>) -> String { match v { None => "undefined".into(), Some(x) => js_string(x) } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

// Slug pattern: prefix-DESC-WITH-DASHES — /\b[A-Z]{2,}[A-Z0-9]*(?:-[A-Z0-9]+)+\b/g
fn slug_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\b[A-Z]{2,}[A-Z0-9]*(?:-[A-Z0-9]+)+\b").unwrap()) }
// Status / continuation intent triggers (each /re/i).
fn pending_triggers() -> &'static Vec<Regex> {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    R.get_or_init(|| vec![
        Regex::new(r"(?i)^(what\s+next|what'?s\s+next)\??$").unwrap(),
        Regex::new(r"(?i)^(go\s+on|continue|carry\s+on|proceed|next)\??$").unwrap(),
        Regex::new(r"(?i)^(status|where\s+are\s+we|what'?s\s+pending|any\s+blockers)\??$").unwrap(),
        Regex::new(r"(?i)^(what\s+do\s+you\s+need|do\s+you\s+have\s+questions?|anything\s+to\s+clarify)\??$").unwrap(),
    ])
}
// start-work intent — the floor MUST defer to the AI classifier when this matches.
fn start_work_patterns() -> &'static Vec<Regex> {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    R.get_or_init(|| vec![
        Regex::new(r"(?i)^\s*(do|start|ship|work\s+on|kick\s+off|begin|let'?s\s+do)\s+\S+").unwrap(),
    ])
}
fn looksLikeStartWork(text: &str) -> bool {
    if text.is_empty() { return false; }
    start_work_patterns().iter().any(|re| re.is_match(text))
}

pub fn extractSlugs(text: &Value) -> Value {
    let s = match text.as_str() { Some(s) if !s.is_empty() => s, _ => return json!([]) };
    let mut out: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for m in slug_re().find_iter(s) {
        let ms = m.as_str().to_string();
        if seen.insert(ms.clone()) { out.push(json!(ms)); }
    }
    json!(out)
}

pub fn hasPendingTrigger(text: &Value) -> Value {
    let s = match text.as_str() { Some(s) if !s.is_empty() => s, _ => return json!(false) };
    let t = s.trim();
    if t.chars().count() > 60 { return json!(false); }
    json!(pending_triggers().iter().any(|re| re.is_match(t)))
}

pub fn queryPending(db: &Connection, slug: &Value) -> Value {
    // slug filter is optional: `(? IS NULL OR s.approved_card_slug = ?)`. try/catch → [] on error.
    let sql = "\
        SELECT s.id AS session_id, \
               s.approved_card_slug AS card_slug, \
               s.status AS session_status, \
               q.id AS question_id, \
               q.question_md, \
               q.allows_writein, \
               q.order_index \
        FROM pl_sessions s \
        JOIN pl_questions q ON q.session_id = s.id \
        LEFT JOIN pl_answers a ON a.question_id = q.id \
        WHERE s.status = 'clarifying' \
          AND a.id IS NULL \
          AND q.step = 'clarify' \
          AND (? IS NULL OR s.approved_card_slug = ?) \
        ORDER BY s.id DESC, q.order_index ASC \
        LIMIT 20";
    crate::db_json::query_rows(db, sql, &[slug.clone(), slug.clone()]).unwrap_or_else(|_| json!([]))
}

pub fn groupByCard(rows: &Value) -> Value {
    let empty: Vec<Value> = Vec::new();
    let rows = rows.as_array().unwrap_or(&empty);
    let mut order: Vec<String> = Vec::new();
    let mut map: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    for r in rows {
        // key = r.card_slug ?? `(session ${r.session_id})`
        let key = match nz(r.get("card_slug")) {
            Some(v) => js_string(v),
            None => format!("(session {})", tmpl(r.get("session_id"))),
        };
        if !map.contains_key(&key) {
            order.push(key.clone());
            let mut g = Map::new();
            if let Some(v) = r.get("card_slug") { g.insert("card_slug".into(), v.clone()); } // undefined → omit
            if let Some(v) = r.get("session_id") { g.insert("session_id".into(), v.clone()); }
            g.insert("questions".into(), json!([]));
            map.insert(key.clone(), Value::Object(g));
        }
        let mut q = Map::new();
        if let Some(v) = r.get("question_id") { q.insert("question_id".into(), v.clone()); }
        if let Some(v) = r.get("question_md") { q.insert("question_md".into(), v.clone()); }
        q.insert("allows_writein".into(), json!(truthy(r.get("allows_writein")))); // !!r.allows_writein
        if let Some(Value::Object(g)) = map.get_mut(&key) {
            if let Some(Value::Array(qs)) = g.get_mut("questions") { qs.push(Value::Object(q)); }
        }
    }
    let out: Vec<Value> = order.iter().map(|k| map.get(k).cloned().unwrap()).collect();
    json!(out)
}

pub fn formatResponse(groups: &Value) -> Value {
    let empty: Vec<Value> = Vec::new();
    let groups = groups.as_array().unwrap_or(&empty);
    let mut lines: Vec<String> = Vec::new();
    let total: usize = groups.iter().map(|g| g.get("questions").and_then(|q| q.as_array()).map_or(0, |a| a.len())).sum();
    lines.push(format!("Pending clarify questions: **{}** across **{}** card(s).", total, groups.len()));
    lines.push(String::new());
    for g in groups {
        let title = match nz(g.get("card_slug")) { Some(v) => tmpl(Some(v)), None => format!("session {}", tmpl(g.get("session_id"))) };
        lines.push(format!("**{}** (session {}):", title, tmpl(g.get("session_id"))));
        if let Some(qs) = g.get("questions").and_then(|q| q.as_array()) {
            for q in qs {
                let qmd = q.get("question_md").and_then(|v| v.as_str()).unwrap_or("").trim();
                lines.push(format!("  - Q{}: {}", tmpl(q.get("question_id")), qmd));
            }
        }
        lines.push(String::new());
    }
    lines.push("Answer by dispatching `pl.session.answer.record` with `{question_id, writein_md}` for each.".into());
    json!(lines.join("\n"))
}

pub fn interviewerPlClarifyFloor(input: &Value, db: &Connection) -> Result<Value, String> {
    let ut_val = match input.get("user_text") { Some(v) => v, None => return Ok(json!({ "matched": false })) };
    // !user_text (falsy) OR typeof !== 'string' → matched:false. db is always present (Connection).
    let s = match ut_val.as_str() { Some(s) if !s.is_empty() => s, _ => return Ok(json!({ "matched": false })) };
    if looksLikeStartWork(s) { return Ok(json!({ "matched": false })); }

    let slugs_v = extractSlugs(ut_val);
    let slugs: Vec<&str> = slugs_v.as_array().map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
    let triggered = matches!(hasPendingTrigger(ut_val), Value::Bool(true));

    if !triggered && slugs.is_empty() { return Ok(json!({ "matched": false })); }

    let mut rows: Vec<Value> = Vec::new();
    if !slugs.is_empty() {
        for slug in &slugs {
            if let Value::Array(a) = queryPending(db, &json!(*slug)) { rows.extend(a); }
        }
    } else if triggered {
        if let Value::Array(a) = queryPending(db, &Value::Null) { rows = a; }
    }
    if rows.is_empty() { return Ok(json!({ "matched": false })); }

    let groups = groupByCard(&json!(rows));
    let response_md = formatResponse(&groups);

    // pending_questions = groups.flatMap(g => g.questions.map(q => ({ card_slug, session_id, question_id, question_md, allows_writein })))
    let mut pending_questions: Vec<Value> = Vec::new();
    if let Some(gs) = groups.as_array() {
        for g in gs {
            let g_card = g.get("card_slug");
            let g_sess = g.get("session_id");
            if let Some(qs) = g.get("questions").and_then(|q| q.as_array()) {
                for q in qs {
                    let mut o = Map::new();
                    if let Some(v) = g_card { o.insert("card_slug".into(), v.clone()); }
                    if let Some(v) = g_sess { o.insert("session_id".into(), v.clone()); }
                    if let Some(v) = q.get("question_id") { o.insert("question_id".into(), v.clone()); }
                    if let Some(v) = q.get("question_md") { o.insert("question_md".into(), v.clone()); }
                    if let Some(v) = q.get("allows_writein") { o.insert("allows_writein".into(), v.clone()); }
                    pending_questions.push(Value::Object(o));
                }
            }
        }
    }

    // next_steps = pending_questions.slice(0,3).map(...)
    let mut next_steps: Vec<Value> = Vec::new();
    for q in pending_questions.iter().take(3) {
        let loc = match nz(q.get("card_slug")) { Some(v) => tmpl(Some(v)), None => format!("session {}", tmpl(q.get("session_id"))) };
        let mut args = Map::new();
        if let Some(v) = q.get("question_id") { args.insert("question_id".into(), v.clone()); }
        args.insert("writein_md".into(), json!("<your answer>"));
        let mut step = Map::new();
        step.insert("hint".into(), json!(format!("answer Q{} ({})", tmpl(q.get("question_id")), loc)));
        step.insert("action_name".into(), json!("pl.session.answer.record"));
        step.insert("args".into(), Value::Object(args));
        next_steps.push(Value::Object(step));
    }

    let referenced = if !slugs.is_empty() { slugs.join(", ") } else { "pending work".to_string() };
    let intent_summary = format!("User referenced {}; surfacing {} unanswered PL clarify question(s)", referenced, pending_questions.len());

    let mut out = Map::new();
    out.insert("matched".into(), json!(true));
    out.insert("response_md".into(), response_md);
    out.insert("intent_summary".into(), json!(intent_summary));
    out.insert("pending_questions".into(), json!(pending_questions));
    out.insert("next_steps".into(), json!(next_steps));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(db: &Connection) {
        db.execute_batch(
            "CREATE TABLE pl_sessions (id INTEGER PRIMARY KEY, approved_card_slug TEXT, status TEXT); \
             CREATE TABLE pl_questions (id INTEGER PRIMARY KEY, session_id INTEGER, step TEXT, question_md TEXT, allows_writein INTEGER, order_index INTEGER); \
             CREATE TABLE pl_answers (id INTEGER PRIMARY KEY, question_id INTEGER); \
             INSERT INTO pl_sessions VALUES (1,'FEAT-FOO','clarifying'); \
             INSERT INTO pl_questions VALUES (10,1,'clarify','What about X?',1,0),(11,1,'clarify','How does Y interact?',0,1);",
        ).unwrap();
    }

    #[test]
    fn helpers() {
        assert_eq!(extractSlugs(&json!("do EPIC-FOO and FEAT-BAR")), json!(["EPIC-FOO", "FEAT-BAR"]));
        assert_eq!(extractSlugs(&json!("lowercase ignored fix-this")), json!([]));
        assert_eq!(extractSlugs(&json!("dup FEAT-X and FEAT-X")), json!(["FEAT-X"])); // dedup
        assert_eq!(extractSlugs(&Value::Null), json!([]));
        assert_eq!(hasPendingTrigger(&json!("what next")), json!(true));
        assert_eq!(hasPendingTrigger(&json!("What next?")), json!(true));
        assert_eq!(hasPendingTrigger(&json!("do you have questions")), json!(true));
        assert_eq!(hasPendingTrigger(&json!("something completely different here please")), json!(false));
        assert_eq!(hasPendingTrigger(&json!("what next is the actual roadmap detailed step by step")), json!(false));
    }

    #[test]
    fn query_and_group_and_format() {
        let db = Connection::open_in_memory().unwrap();
        seed(&db);
        let rows = queryPending(&db, &json!("FEAT-FOO"));
        assert_eq!(rows.as_array().unwrap().len(), 2);
        assert_eq!(rows[0]["card_slug"], json!("FEAT-FOO"));
        assert_eq!(rows[0]["question_id"], json!(10));
        assert_eq!(queryPending(&db, &json!("NOPE")).as_array().unwrap().len(), 0);
        assert_eq!(queryPending(&db, &Value::Null).as_array().unwrap().len(), 2); // slug null → all

        let groups = groupByCard(&rows);
        assert_eq!(groups.as_array().unwrap().len(), 1);
        assert_eq!(groups[0]["card_slug"], json!("FEAT-FOO"));
        assert_eq!(groups[0]["questions"].as_array().unwrap().len(), 2);
        assert_eq!(groups[0]["questions"][0]["allows_writein"], json!(true)); // 1 → true
        assert_eq!(groups[0]["questions"][1]["allows_writein"], json!(false)); // 0 → false

        let md = formatResponse(&groups);
        let md = md.as_str().unwrap();
        assert!(md.contains("Pending clarify questions: **2** across **1** card(s)."));
        assert!(md.contains("**FEAT-FOO** (session 1):"));
        assert!(md.contains("  - Q10: What about X?"));
        assert!(md.contains("pl.session.answer.record"));
    }

    #[test]
    fn floor_default() {
        let db = Connection::open_in_memory().unwrap();
        seed(&db);
        // empty / non-string → matched false
        assert_eq!(interviewerPlClarifyFloor(&json!({}), &db).unwrap(), json!({ "matched": false }));
        assert_eq!(interviewerPlClarifyFloor(&json!({ "user_text": "" }), &db).unwrap(), json!({ "matched": false }));
        assert_eq!(interviewerPlClarifyFloor(&json!({ "user_text": "hi" }), &db).unwrap(), json!({ "matched": false })); // no slug, no trigger
        // start-work defers
        assert_eq!(interviewerPlClarifyFloor(&json!({ "user_text": "do FEAT-FOO" }), &db).unwrap(), json!({ "matched": false }));
        // slug mention fires
        let r = interviewerPlClarifyFloor(&json!({ "user_text": "go on with FEAT-FOO" }), &db).unwrap();
        assert_eq!(r["matched"], json!(true));
        assert_eq!(r["pending_questions"].as_array().unwrap().len(), 2);
        assert_eq!(r["pending_questions"][0]["card_slug"], json!("FEAT-FOO"));
        assert_eq!(r["next_steps"].as_array().unwrap().len(), 2);
        assert_eq!(r["next_steps"][0]["action_name"], json!("pl.session.answer.record"));
        assert_eq!(r["next_steps"][0]["args"]["question_id"], json!(10));
        assert!(r["intent_summary"].as_str().unwrap().contains("FEAT-FOO"));
        // trigger phrase, no slug → all pending
        let t = interviewerPlClarifyFloor(&json!({ "user_text": "what next" }), &db).unwrap();
        assert_eq!(t["matched"], json!(true));
        assert!(t["intent_summary"].as_str().unwrap().contains("pending work"));
    }
}
