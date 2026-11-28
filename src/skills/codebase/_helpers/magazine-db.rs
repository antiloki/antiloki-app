//! magazine-db.rs — twin of skills/codebase/_helpers/magazine-db.mjs (helper module, all
//! db-first). NAMES ARE CONTRACT: ensureMagazineIssues · facultyBrief · aiMeta · latestIssue ·
//! listIssues · appendIssue · ensureFacultyCourses · latestCourse · listCourses · appendCourse.
//! The faculty magazines' issue archive: issues/courses are APPEND-ONLY editions in
//! knowledge.magazine_issues / knowledge.faculty_courses (runtime-ensured, not in the schema
//! baseline). facultyBrief distills the latest PRESSED issue + course into grounded markdown —
//! the one corpus seam every chat/pane/studio faculty context reads through.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
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
// String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// String(v) (no ??)
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
// `${v}` template interpolation
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
// JS `??` right operand
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
// JS Number(v)
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
// node:sqlite binds an integral JS number as INTEGER, a fractional one as REAL.
fn num_param(n: f64) -> Value { if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) } }

// `const parse = (t, fallback) => { try { return JSON.parse(t); } catch { return fallback; } }`
// specialized to the 3 call sites here, which all pass `fallback = null`: a parse SUCCESS that
// itself yields `null` and a parse FAILURE both collapse to the same Value::Null, so this needs
// no separate fallback parameter.
fn parse_or_null(t: Option<&str>) -> Value {
    t.and_then(|s| serde_json::from_str::<Value>(s).ok()).unwrap_or(Value::Null)
}

fn ws_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\s+").unwrap()) }
// `.replace(/\s+/g, ' ')` — collapse whitespace runs, NO trim (unlike bugfinder/_helpers'
// ws_collapse, the two call sites here never trim the result).
fn ws_collapse_notrim(s: &str) -> String { ws_re().replace_all(s, " ").to_string() }

const MAGAZINE_ISSUES_SQL: &str = "CREATE TABLE IF NOT EXISTS knowledge.magazine_issues (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    faculty TEXT NOT NULL,
    issue_no INTEGER NOT NULL,
    observation_hash TEXT NOT NULL,
    observation_json TEXT NOT NULL,
    editorial_json TEXT NOT NULL,
    model_id TEXT,
    pressed_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(faculty, issue_no)
  )";

const FACULTY_COURSES_SQL: &str = "CREATE TABLE IF NOT EXISTS knowledge.faculty_courses (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    faculty TEXT NOT NULL,
    course_no INTEGER NOT NULL,
    source_hash TEXT NOT NULL,
    course_json TEXT NOT NULL,
    model_id TEXT,
    pressed_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(faculty, course_no)
  )";

// FROZEN-CLOCK MIRROR: the executed copy pins datetime('now') to the frozen instant (cockpit-schema
// precedent) so a later twin's INSERT (which relies on the pressed_at DEFAULT) stays deterministic.
pub fn ensureMagazineIssues(db: &Connection) -> Value {
    let _ = db.execute_batch(&MAGAZINE_ISSUES_SQL.replace("datetime('now'", &format!("datetime({}", crate::clock::now_sql_inner())));
    Value::Null
}

// courses are PER TOPIC — defensive ALTER so live DBs that minted the table pre-topic gain the
// column on the next open; the error (column already exists) is swallowed, exactly like the JS try/catch.
pub fn ensureFacultyCourses(db: &Connection) -> Value {
    let _ = db.execute_batch(&FACULTY_COURSES_SQL.replace("datetime('now'", &format!("datetime({}", crate::clock::now_sql_inner())));
    let _ = db.execute_batch("ALTER TABLE knowledge.faculty_courses ADD COLUMN topic TEXT");
    Value::Null
}

/// `export function aiMeta(db, res)` — AI transparency: distill an ai-call result + its
/// brain.ai_calls row (unqualified `ai_calls` — SQLite resolves it against the attached `brain`
/// schema since no such table exists in main) into the meta block every press returns to the FE.
pub fn aiMeta(db: &Connection, res: &Value) -> Value {
    if !truthy(Some(res)) { return Value::Null; }
    let ai_call_id = res.get("ai_call_id");
    let mut tokens_in = Value::Null;
    let mut tokens_out = Value::Null;
    if truthy(ai_call_id) {
        if let Ok(rows) = crate::db_json::query_rows(db, "SELECT tokens_in, tokens_out FROM ai_calls WHERE id = ?", &[ai_call_id.cloned().unwrap_or(Value::Null)]) {
            if let Some(r) = rows.as_array().and_then(|a| a.first()) {
                tokens_in = r.get("tokens_in").cloned().unwrap_or(Value::Null);
                tokens_out = r.get("tokens_out").cloned().unwrap_or(Value::Null);
            }
        }
    }
    json!({
        "ai_call_id": nz(ai_call_id).cloned().unwrap_or(Value::Null),
        "model_id": nz(res.get("model_id")).cloned().unwrap_or(Value::Null),
        "provider": nz(res.get("provider")).cloned().unwrap_or(Value::Null),
        "tier": nz(res.get("tier_used")).cloned().unwrap_or(Value::Null),
        "cost_usd": nz(res.get("cost_usd")).cloned().unwrap_or(Value::Null),
        "duration_ms": nz(res.get("duration_ms")).cloned().unwrap_or(Value::Null),
        "cached": json!(truthy(res.get("cached"))),
        "tokens_in": tokens_in,
        "tokens_out": tokens_out,
    })
}

pub fn latestIssue(db: &Connection, faculty: &Value) -> Value {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT issue_no, observation_hash, observation_json, editorial_json, model_id, pressed_at FROM knowledge.magazine_issues WHERE faculty = ? ORDER BY issue_no DESC LIMIT 1",
        &[faculty.clone()],
    ).unwrap_or_else(|_| json!([]));
    match rows.as_array().and_then(|a| a.first()) {
        None => Value::Null,
        Some(row) => json!({
            "issue_no": row.get("issue_no").cloned().unwrap_or(Value::Null),
            "observation_hash": row.get("observation_hash").cloned().unwrap_or(Value::Null),
            "observation": parse_or_null(row.get("observation_json").and_then(|v| v.as_str())),
            "editorial": parse_or_null(row.get("editorial_json").and_then(|v| v.as_str())),
            "model_id": nz(row.get("model_id")).cloned().unwrap_or(Value::Null),
            "pressed_at": row.get("pressed_at").cloned().unwrap_or(Value::Null),
        }),
    }
}

/// `export function listIssues(db, faculty, limit = 12)` — the JS default only fires when the
/// call omits the argument (`undefined`); production always passes `limit` explicitly, so the
/// twin resolves `limit` as a concrete number (`Math.max(1, limit)`).
pub fn listIssues(db: &Connection, faculty: &Value, limit: &Value) -> Value {
    let lim = js_number(Some(limit)).max(1.0);
    crate::db_json::query_rows(
        db,
        "SELECT issue_no, observation_hash, pressed_at FROM knowledge.magazine_issues WHERE faculty = ? ORDER BY issue_no DESC LIMIT ?",
        &[faculty.clone(), num_param(lim)],
    ).unwrap_or_else(|_| json!([]))
}

pub fn appendIssue(db: &Connection, opts: &Value) -> Value {
    let faculty = opts.get("faculty").cloned().unwrap_or(Value::Null);
    let max_rows = crate::db_json::query_rows(db, "SELECT MAX(issue_no) AS m FROM knowledge.magazine_issues WHERE faculty = ?", &[faculty.clone()])
        .unwrap_or_else(|_| json!([]));
    let m = max_rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("m")).cloned().unwrap_or(Value::Null);
    let base = if m.is_null() { 0.0 } else { js_number(Some(&m)) };
    let issue_no = num_param(base + 1.0);

    let observation_hash = opts.get("observation_hash").cloned().unwrap_or(Value::Null);
    let observation_v = nz(opts.get("observation")).cloned().unwrap_or_else(|| json!({}));
    let editorial_v = nz(opts.get("editorial")).cloned().unwrap_or_else(|| json!({}));
    let observation_json = json!(serde_json::to_string(&observation_v).unwrap_or_else(|_| "{}".into()));
    let editorial_json = json!(serde_json::to_string(&editorial_v).unwrap_or_else(|_| "{}".into()));
    let model_id = opts.get("model_id").cloned().unwrap_or(Value::Null);

    crate::db_json::run_write(
        db,
        "INSERT INTO knowledge.magazine_issues (faculty, issue_no, observation_hash, observation_json, editorial_json, model_id) VALUES (?,?,?,?,?,?)",
        &[faculty.clone(), issue_no.clone(), observation_hash, observation_json, editorial_json, model_id],
    ).unwrap();
    let rows = crate::db_json::query_rows(
        db,
        "SELECT issue_no, observation_hash, pressed_at FROM knowledge.magazine_issues WHERE faculty = ? AND issue_no = ?",
        &[faculty, issue_no],
    ).unwrap();
    rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null)
}

pub fn latestCourse(db: &Connection, faculty: &Value, topic: &Value) -> Value {
    let rows = if truthy(Some(topic)) {
        crate::db_json::query_rows(
            db,
            "SELECT course_no, topic, source_hash, course_json, model_id, pressed_at FROM knowledge.faculty_courses WHERE faculty = ? AND topic = ? ORDER BY course_no DESC LIMIT 1",
            &[faculty.clone(), topic.clone()],
        )
    } else {
        crate::db_json::query_rows(
            db,
            "SELECT course_no, topic, source_hash, course_json, model_id, pressed_at FROM knowledge.faculty_courses WHERE faculty = ? ORDER BY course_no DESC LIMIT 1",
            &[faculty.clone()],
        )
    }.unwrap_or_else(|_| json!([]));
    match rows.as_array().and_then(|a| a.first()) {
        None => Value::Null,
        Some(row) => json!({
            "course_no": row.get("course_no").cloned().unwrap_or(Value::Null),
            "topic": nz(row.get("topic")).cloned().unwrap_or(Value::Null),
            "source_hash": row.get("source_hash").cloned().unwrap_or(Value::Null),
            "course": parse_or_null(row.get("course_json").and_then(|v| v.as_str())),
            "model_id": nz(row.get("model_id")).cloned().unwrap_or(Value::Null),
            "pressed_at": row.get("pressed_at").cloned().unwrap_or(Value::Null),
        }),
    }
}

/// `export function listCourses(db, faculty, limit = 12)` — see listIssues note on the default.
pub fn listCourses(db: &Connection, faculty: &Value, limit: &Value) -> Value {
    let lim = js_number(Some(limit)).max(1.0);
    crate::db_json::query_rows(
        db,
        "SELECT course_no, topic, source_hash, pressed_at FROM knowledge.faculty_courses WHERE faculty = ? ORDER BY course_no DESC LIMIT ?",
        &[faculty.clone(), num_param(lim)],
    ).unwrap_or_else(|_| json!([]))
}

pub fn appendCourse(db: &Connection, opts: &Value) -> Value {
    let faculty = opts.get("faculty").cloned().unwrap_or(Value::Null);
    let max_rows = crate::db_json::query_rows(db, "SELECT MAX(course_no) AS m FROM knowledge.faculty_courses WHERE faculty = ?", &[faculty.clone()])
        .unwrap_or_else(|_| json!([]));
    let m = max_rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("m")).cloned().unwrap_or(Value::Null);
    let base = if m.is_null() { 0.0 } else { js_number(Some(&m)) };
    let course_no = num_param(base + 1.0);

    let topic = opts.get("topic").cloned().unwrap_or(Value::Null);
    let source_hash = opts.get("source_hash").cloned().unwrap_or(Value::Null);
    let course_v = nz(opts.get("course")).cloned().unwrap_or_else(|| json!({}));
    let course_json = json!(serde_json::to_string(&course_v).unwrap_or_else(|_| "{}".into()));
    let model_id = opts.get("model_id").cloned().unwrap_or(Value::Null);

    crate::db_json::run_write(
        db,
        "INSERT INTO knowledge.faculty_courses (faculty, course_no, topic, source_hash, course_json, model_id) VALUES (?,?,?,?,?,?)",
        &[faculty.clone(), course_no.clone(), topic, source_hash, course_json, model_id],
    ).unwrap();
    let rows = crate::db_json::query_rows(
        db,
        "SELECT course_no, topic, source_hash, pressed_at FROM knowledge.faculty_courses WHERE faculty = ? AND course_no = ?",
        &[faculty, course_no],
    ).unwrap();
    rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null)
}

/// `export function facultyBrief(db, faculty)` — the faculty as a CONTEXT SOURCE: the brief is
/// built from the latest PRESSED issue's editorial + stored observation, plus the latest course.
pub fn facultyBrief(db: &Connection, faculty: &Value) -> Value {
    ensureMagazineIssues(db);
    ensureFacultyCourses(db);
    let mag = if faculty.as_str() == Some("arch") { "THE STACK GAZETTE" } else { "THE MARKET HERALD" };
    let issue = latestIssue(db, faculty);
    if issue.is_null() {
        return json!(format!(
            "{} ({} faculty): no issue pressed yet — open the faculty and press issue Nº1 to publish its knowledge.",
            mag, tmpl(Some(faculty))
        ));
    }

    let mut l: Vec<String> = Vec::new();
    let pressed_10: String = str_qq(nz(issue.get("pressed_at"))).chars().take(10).collect();
    l.push(format!("{} — issue Nº{}, pressed {}", mag, tmpl(issue.get("issue_no")), pressed_10));

    let empty_obj = json!({});
    let empty_arr = json!([]);

    let ed = nz(issue.get("editorial")).unwrap_or(&empty_obj);
    let cover = ed.get("cover");
    if truthy(cover.and_then(|c| c.get("headline"))) {
        let headline = tmpl(cover.and_then(|c| c.get("headline")));
        let standfirst = cover.and_then(|c| c.get("standfirst"));
        let sf = if truthy(standfirst) { format!(" — {}", tmpl(standfirst)) } else { String::new() };
        l.push(format!("COVER: {}{}", headline, sf));
    }
    if truthy(ed.get("letter_md")) {
        let letter = ws_collapse_notrim(&js_string(ed.get("letter_md").unwrap()));
        let letter_600: String = letter.chars().take(600).collect();
        l.push(format!("EDITOR'S LETTER: {}", letter_600));
    }

    let d = nz(issue.get("observation")).unwrap_or(&empty_obj);

    let deps: Vec<Value> = nz(d.get("ledger").and_then(|v| v.get("deps"))).unwrap_or(&empty_arr).as_array().cloned().unwrap_or_default();
    let deps10: Vec<&Value> = deps.iter().take(10).collect();
    if !deps10.is_empty() {
        let parts: Vec<String> = deps10.iter().map(|dep| {
            let current = dep.get("current");
            let latest = dep.get("latest");
            let arrow = if truthy(latest) && latest != current { format!("→{}", tmpl(latest)) } else { String::new() };
            let adv_len = nz(dep.get("advisories")).and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
            let adv = if adv_len > 0 { format!(" [{} adv]", adv_len) } else { String::new() };
            format!("{} {}{}{}", tmpl(dep.get("name")), tmpl(current), arrow, adv)
        }).collect();
        l.push(format!("LEDGER (top by risk): {}", parts.join(" · ")));
    }

    let hz: Vec<Value> = nz(d.get("horizon").and_then(|v| v.get("products"))).unwrap_or(&empty_arr).as_array().cloned().unwrap_or_default();
    if !hz.is_empty() {
        let parts: Vec<String> = hz.iter().map(|p| {
            let runway = p.get("runway_days");
            let rw = if nz(runway).is_some() { format!(" ({}d)", tmpl(runway)) } else { String::new() };
            format!("{} cycle {} eol {}{}", tmpl(p.get("product")), tmpl(p.get("cycle")), tmpl(p.get("eol")), rw)
        }).collect();
        l.push(format!("HORIZON: {}", parts.join(" · ")));
    }

    let st: Vec<Value> = nz(d.get("stats").and_then(|v| v.get("techs"))).unwrap_or(&empty_arr).as_array().cloned().unwrap_or_default();
    if !st.is_empty() {
        let parts: Vec<String> = st.iter().take(8).map(|t| {
            let weekly = t.get("weekly");
            let stars = t.get("stars");
            let rel90 = t.get("releases_90d");
            let w = if nz(weekly).is_some() { format!("{}/wk", tmpl(weekly)) } else { String::new() };
            let s = if nz(stars).is_some() { format!(" ★{}", tmpl(stars)) } else { String::new() };
            let r = if nz(rel90).is_some() { format!(" rel90:{}", tmpl(rel90)) } else { String::new() };
            format!("{} {}{}{}", tmpl(t.get("name")), w, s, r)
        }).collect();
        l.push(format!("WORLD NUMBERS: {}", parts.join(" · ")));
    }

    let press_items: Vec<Value> = nz(d.get("press").and_then(|v| v.get("items"))).unwrap_or(&empty_arr).as_array().cloned().unwrap_or_default();
    for it in press_items.iter().take(5) {
        let content = it.get("content");
        let c = if truthy(content) {
            let collapsed = ws_collapse_notrim(&js_string(content.unwrap()));
            format!(" — {}", collapsed.chars().take(200).collect::<String>())
        } else { String::new() };
        l.push(format!("TECH PRESS: [{}] {}{}", tmpl(it.get("source")), tmpl(it.get("title")), c));
    }

    let rivals: Vec<Value> = nz(d.get("rivals").and_then(|v| v.get("rivals"))).unwrap_or(&empty_arr).as_array().cloned().unwrap_or_default();
    if !rivals.is_empty() {
        let parts: Vec<String> = rivals.iter().map(|r| {
            let claims: Vec<String> = nz(r.get("claims")).and_then(|v| v.as_array()).cloned().unwrap_or_default()
                .iter().take(3).map(|c| tmpl(Some(c))).collect();
            format!("{} (\"{}\") claims: {}", tmpl(r.get("name")), tmpl(r.get("title")), claims.join(" | "))
        }).collect();
        l.push(format!("RIVALS: {}", parts.join(" · ")));
    }

    let dispatch_items: Vec<Value> = nz(d.get("dispatches").and_then(|v| v.get("items"))).unwrap_or(&empty_arr).as_array().cloned().unwrap_or_default();
    for it in dispatch_items.iter().take(5) {
        l.push(format!("FIELD: [{}] {}", tmpl(it.get("source")), tmpl(it.get("title"))));
    }

    let desk_items: Vec<Value> = nz(d.get("desk").and_then(|v| v.get("items"))).unwrap_or(&empty_arr).as_array().cloned().unwrap_or_default();
    if !desk_items.is_empty() {
        let labels: Vec<String> = desk_items.iter().map(|it| tmpl(it.get("label"))).collect();
        l.push(format!("DESK MATERIAL: {}", labels.join(" · ")));
    }

    let course = latestCourse(db, faculty, &Value::Null);
    let course_course = nz(course.get("course"));
    if truthy(course_course) {
        let cc = course_course.unwrap();
        let modules: Vec<String> = nz(cc.get("modules")).and_then(|v| v.as_array()).cloned().unwrap_or_default()
            .iter().map(|m| tmpl(m.get("tech"))).collect();
        let topic = match nz(course.get("topic")) { Some(v) => tmpl(Some(v)), None => "the stack".to_string() };
        l.push(format!("LATEST COURSE: \"{}\" ({}) — {}", tmpl(cc.get("title")), topic, modules.join(", ")));
    }

    json!(l.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS knowledge;").unwrap();
        c
    }

    #[test]
    fn ensure_creates_tables_idempotently_and_freezes_defaults() {
        let c = fresh();
        assert_eq!(ensureMagazineIssues(&c), Value::Null);
        assert_eq!(ensureMagazineIssues(&c), Value::Null); // idempotent
        assert_eq!(ensureFacultyCourses(&c), Value::Null);
        assert_eq!(ensureFacultyCourses(&c), Value::Null); // idempotent, ALTER swallowed 2nd time
        c.execute(
            "INSERT INTO knowledge.magazine_issues (faculty, issue_no, observation_hash, observation_json, editorial_json) VALUES ('arch', 1, 'h', '{}', '{}')",
            [],
        ).unwrap();
        let pressed: String = c.query_row("SELECT pressed_at FROM knowledge.magazine_issues WHERE faculty='arch'", [], |r| r.get(0)).unwrap();
        assert_eq!(pressed, "2026-01-01 00:00:00");
        // topic column exists (defensive ALTER landed)
        c.execute(
            "INSERT INTO knowledge.faculty_courses (faculty, course_no, topic, source_hash, course_json) VALUES ('arch', 1, 't', 'h', '{}')",
            [],
        ).unwrap();
        let topic: String = c.query_row("SELECT topic FROM knowledge.faculty_courses WHERE faculty='arch'", [], |r| r.get(0)).unwrap();
        assert_eq!(topic, "t");
    }

    #[test]
    fn ai_meta_branches() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, tokens_in INTEGER, tokens_out INTEGER); \
            INSERT INTO brain.ai_calls (id, tokens_in, tokens_out) VALUES (5, 10, 20);").unwrap();

        assert_eq!(aiMeta(&c, &Value::Null), Value::Null);

        let found = aiMeta(&c, &json!({ "ai_call_id": 5, "model_id": "m1", "provider": "anthropic", "tier_used": "mid", "cost_usd": 0.03, "duration_ms": 120, "cached": true }));
        assert_eq!(found, json!({ "ai_call_id": 5, "model_id": "m1", "provider": "anthropic", "tier": "mid", "cost_usd": 0.03, "duration_ms": 120, "cached": true, "tokens_in": 10, "tokens_out": 20 }));

        let missing = aiMeta(&c, &json!({ "ai_call_id": 999 }));
        assert_eq!(missing["tokens_in"], Value::Null);
        assert_eq!(missing["tokens_out"], Value::Null);
        assert_eq!(missing["ai_call_id"], json!(999));

        let absent = aiMeta(&c, &json!({}));
        assert_eq!(absent, json!({ "ai_call_id": Value::Null, "model_id": Value::Null, "provider": Value::Null, "tier": Value::Null, "cost_usd": Value::Null, "duration_ms": Value::Null, "cached": false, "tokens_in": Value::Null, "tokens_out": Value::Null }));
    }

    #[test]
    fn latest_and_list_issues() {
        let c = fresh();
        ensureMagazineIssues(&c);
        c.execute_batch(
            "INSERT INTO knowledge.magazine_issues (faculty, issue_no, observation_hash, observation_json, editorial_json, model_id, pressed_at) VALUES \
             ('arch', 1, 'h1', '{\"a\":1}', '{\"b\":2}', 'gpt', '2026-01-01 00:00:00'), \
             ('arch', 2, 'h2', 'not json', 'null', NULL, '2026-01-02 00:00:00');",
        ).unwrap();

        let latest = latestIssue(&c, &json!("arch"));
        assert_eq!(latest["issue_no"], json!(2));
        assert_eq!(latest["observation"], Value::Null); // invalid JSON -> fallback null
        assert_eq!(latest["editorial"], Value::Null); // JSON 'null' parses to null too
        assert_eq!(latest["model_id"], Value::Null);

        assert_eq!(latestIssue(&c, &json!("none")), Value::Null);

        let listed = listIssues(&c, &json!("arch"), &json!(1));
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!(listed[0]["issue_no"], json!(2));

        // Math.max(1, limit) clamp
        let clamped = listIssues(&c, &json!("arch"), &json!(-5));
        assert_eq!(clamped.as_array().unwrap().len(), 1);
    }

    #[test]
    fn append_issue_increments_and_defaults() {
        let c = fresh();
        ensureMagazineIssues(&c);
        let r1 = appendIssue(&c, &json!({ "faculty": "arch", "observation_hash": "h1", "observation": { "x": 1 }, "editorial": { "y": 2 }, "model_id": "gpt" }));
        assert_eq!(r1["issue_no"], json!(1));

        let r2 = appendIssue(&c, &json!({ "faculty": "arch", "observation_hash": "h2" })); // observation/editorial/model_id absent
        assert_eq!(r2["issue_no"], json!(2));
        let stored: (String, String, Option<String>) = c.query_row(
            "SELECT observation_json, editorial_json, model_id FROM knowledge.magazine_issues WHERE issue_no = 2",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        assert_eq!(stored.0, "{}");
        assert_eq!(stored.1, "{}");
        assert_eq!(stored.2, None);
    }

    #[test]
    fn latest_and_list_courses() {
        let c = fresh();
        ensureFacultyCourses(&c);
        c.execute_batch(
            "INSERT INTO knowledge.faculty_courses (faculty, course_no, topic, source_hash, course_json, pressed_at) VALUES \
             ('arch', 1, 'vite', 'h1', '{\"title\":\"V1\"}', '2026-01-01 00:00:00'), \
             ('arch', 2, 'rust', 'h2', '{\"title\":\"V2\"}', '2026-01-02 00:00:00');",
        ).unwrap();

        let by_topic = latestCourse(&c, &json!("arch"), &json!("vite"));
        assert_eq!(by_topic["course_no"], json!(1));
        assert_eq!(by_topic["course"], json!({ "title": "V1" }));

        let any_topic = latestCourse(&c, &json!("arch"), &Value::Null);
        assert_eq!(any_topic["course_no"], json!(2)); // latest overall, no topic filter

        assert_eq!(latestCourse(&c, &json!("arch"), &json!("nope")), Value::Null);
        assert_eq!(latestCourse(&c, &json!("none"), &Value::Null), Value::Null);

        let listed = listCourses(&c, &json!("arch"), &json!(1));
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!(listed[0]["course_no"], json!(2));
    }

    #[test]
    fn append_course_increments_and_defaults() {
        let c = fresh();
        ensureFacultyCourses(&c);
        let r1 = appendCourse(&c, &json!({ "faculty": "arch", "topic": "vite", "source_hash": "h1", "course": { "title": "C1" } }));
        assert_eq!(r1["course_no"], json!(1));
        assert_eq!(r1["topic"], json!("vite"));

        let r2 = appendCourse(&c, &json!({ "faculty": "arch", "source_hash": "h2" })); // topic/course/model_id absent
        assert_eq!(r2["course_no"], json!(2));
        let stored: (Option<String>, String) = c.query_row(
            "SELECT topic, course_json FROM knowledge.faculty_courses WHERE course_no = 2",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(stored.0, None);
        assert_eq!(stored.1, "{}");
    }

    #[test]
    fn faculty_brief_unpressed_and_full() {
        let c = fresh();
        let brief0 = facultyBrief(&c, &json!("arch"));
        assert!(brief0.as_str().unwrap().contains("THE STACK GAZETTE (arch faculty): no issue pressed yet"));

        let brief_other = facultyBrief(&c, &json!("biz"));
        assert!(brief_other.as_str().unwrap().starts_with("THE MARKET HERALD (biz faculty)"));

        let observation = json!({
            "ledger": { "deps": [{ "name": "vite", "current": "5.0.0", "latest": "8.0.0", "behind": 100, "advisories": [{ "id": "GHSA-X" }] }] },
            "horizon": { "products": [{ "product": "nodejs", "cycle": "24", "eol": "2028-04-30", "runway_days": 600 }] },
            "stats": { "techs": [{ "name": "vite", "weekly": 1000000, "stars": 70000, "releases_90d": 5 }] },
            "press": { "items": [{ "source": "vite", "title": "Vite 8 is out", "content": "The bundler moves to native ESM everywhere." }] },
        });
        let editorial = json!({ "cover": { "headline": "Two majors of runway debt" }, "letter_md": "The ledger names the bump." });
        appendIssue(&c, &json!({ "faculty": "arch", "observation_hash": "h1", "observation": observation, "editorial": editorial }));
        appendCourse(&c, &json!({ "faculty": "arch", "topic": "vite", "source_hash": "h1", "course": { "title": "Vite, taught by its docs", "modules": [{ "tech": "vite" }] } }));

        let brief = facultyBrief(&c, &json!("arch"));
        let s = brief.as_str().unwrap();
        assert!(s.contains("THE STACK GAZETTE — issue Nº1"));
        assert!(s.contains("COVER: Two majors of runway debt"));
        assert!(s.contains("vite 5.0.0→8.0.0 [1 adv]"));
        assert!(s.contains("nodejs cycle 24"));
        assert!(s.contains("1000000/wk"));
        assert!(s.contains("Vite 8 is out"));
        assert!(s.contains("LATEST COURSE: \"Vite, taught by its docs\" (vite)"));
    }

    #[test]
    fn faculty_brief_minimal_no_course() {
        let c = fresh();
        ensureMagazineIssues(&c); // appendIssue has no self-heal (unlike facultyBrief) — table must pre-exist
        appendIssue(&c, &json!({ "faculty": "law", "observation_hash": "h1" })); // observation/editorial default {}
        let brief = facultyBrief(&c, &json!("law"));
        let s = brief.as_str().unwrap();
        assert_eq!(s, "THE MARKET HERALD — issue Nº1, pressed 2026-01-01");
    }
}
