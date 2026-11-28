//! course-get.rs — twin of skills/codebase/course-get.mjs (db READ lane). NAMES ARE CONTRACT:
//! `courseGet` (named default). Reads a faculty's latest pressed course (per TOPIC when given) +
//! the archive list (expeditions v4). Pure read; the FE course wizard opens from this.
use rusqlite::Connection;
use serde_json::{json, Value};

// FACULTIES inlined from skills/codebase/magazine-observe.mjs `export const FACULTIES = new
// Set(['arch', 'product'])` — that sibling module is not yet a converted twin, so the enum values
// are transcribed directly rather than imported (per playbook instruction for this file).
const FACULTIES: [&str; 2] = ["arch", "product"];

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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

pub fn courseGet(input: &Value, db: &Connection) -> Result<Value, String> {
    // context.db is always injected live by the db lane — unreachable in the harness, kept for parity.
    let faculty = str_qq(nz(input.get("faculty"))).trim().to_string();
    if !FACULTIES.contains(&faculty.as_str()) {
        return Err(format!( // guard-throw, not fixture-exercised
            "course-get: faculty must be one of: {} (got '{}')",
            FACULTIES.join(", "), faculty
        ));
    }
    let topic_raw = str_qq(nz(input.get("topic"))).trim().to_lowercase();
    let topic: Value = if topic_raw.is_empty() { Value::Null } else { json!(topic_raw) };

    crate::codebase__helpers_magazine_db::ensureFacultyCourses(db);
    let faculty_v = json!(faculty);
    let latest = crate::codebase__helpers_magazine_db::latestCourse(db, &faculty_v, &topic);

    let course = if latest.is_null() {
        Value::Null
    } else {
        let course_obj = nz(latest.get("course")).cloned().unwrap_or_else(|| json!({}));
        let out = course_obj.as_object().cloned().unwrap_or_default();
        // spread ...latest.course AFTER the leading fields, matching the JS object literal's
        // insertion order: course_no, topic, pressed_at, source_hash, then the spread keys.
        let mut ordered = serde_json::Map::new();
        ordered.insert("course_no".into(), latest.get("course_no").cloned().unwrap_or(Value::Null));
        ordered.insert("topic".into(), latest.get("topic").cloned().unwrap_or(Value::Null));
        ordered.insert("pressed_at".into(), latest.get("pressed_at").cloned().unwrap_or(Value::Null));
        ordered.insert("source_hash".into(), latest.get("source_hash").cloned().unwrap_or(Value::Null));
        for (k, v) in out.into_iter() { ordered.insert(k, v); }
        Value::Object(ordered)
    };

    let courses = crate::codebase__helpers_magazine_db::listCourses(db, &faculty_v, &json!(12));

    Ok(json!({
        "ok": true,
        "course": course,
        "courses": courses,
    }))
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
    fn no_course_pressed_yet() {
        let c = fresh();
        let r = courseGet(&json!({ "faculty": "arch" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["course"], Value::Null);
        assert_eq!(r["courses"], json!([]));
    }

    #[test]
    fn latest_course_no_topic_filter() {
        let c = fresh();
        crate::codebase__helpers_magazine_db::ensureFacultyCourses(&c);
        c.execute_batch(
            "INSERT INTO knowledge.faculty_courses (faculty, course_no, topic, source_hash, course_json, pressed_at) VALUES \
             ('arch', 1, 'vite', 'h1', '{\"title\":\"V1\",\"modules\":[{\"tech\":\"vite\"}]}', '2026-01-01 00:00:00'), \
             ('arch', 2, 'rust', 'h2', '{\"title\":\"V2\",\"modules\":[{\"tech\":\"rust\"}]}', '2026-01-02 00:00:00');",
        ).unwrap();

        let r = courseGet(&json!({ "faculty": "arch" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["course"]["course_no"], json!(2)); // latest overall, no topic
        assert_eq!(r["course"]["topic"], json!("rust"));
        assert_eq!(r["course"]["pressed_at"], json!("2026-01-02 00:00:00"));
        assert_eq!(r["course"]["source_hash"], json!("h2"));
        assert_eq!(r["course"]["title"], json!("V2"));
        assert_eq!(r["course"]["modules"], json!([{ "tech": "rust" }]));
        assert_eq!(r["courses"].as_array().unwrap().len(), 2);
        assert_eq!(r["courses"][0]["course_no"], json!(2)); // ORDER BY course_no DESC
    }

    #[test]
    fn topic_filter_narrows_and_lowercases_trims() {
        let c = fresh();
        crate::codebase__helpers_magazine_db::ensureFacultyCourses(&c);
        c.execute_batch(
            "INSERT INTO knowledge.faculty_courses (faculty, course_no, topic, source_hash, course_json, pressed_at) VALUES \
             ('arch', 1, 'vite', 'h1', '{\"title\":\"V1\"}', '2026-01-01 00:00:00'), \
             ('arch', 2, 'rust', 'h2', '{\"title\":\"V2\"}', '2026-01-02 00:00:00');",
        ).unwrap();

        let r = courseGet(&json!({ "faculty": "arch", "topic": "  ViTe  " }), &c).unwrap();
        assert_eq!(r["course"]["course_no"], json!(1));
        assert_eq!(r["course"]["topic"], json!("vite"));
    }

    #[test]
    fn topic_not_found_yields_null_course() {
        let c = fresh();
        crate::codebase__helpers_magazine_db::ensureFacultyCourses(&c);
        c.execute_batch(
            "INSERT INTO knowledge.faculty_courses (faculty, course_no, topic, source_hash, course_json, pressed_at) VALUES \
             ('arch', 1, 'vite', 'h1', '{\"title\":\"V1\"}', '2026-01-01 00:00:00');",
        ).unwrap();
        let r = courseGet(&json!({ "faculty": "arch", "topic": "nope" }), &c).unwrap();
        assert_eq!(r["course"], Value::Null);
        assert_eq!(r["courses"].as_array().unwrap().len(), 1); // archive list ignores topic filter
    }

    #[test]
    fn invalid_faculty_errors() {
        let c = fresh();
        let e = courseGet(&json!({ "faculty": "biz" }), &c).unwrap_err();
        assert!(e.contains("faculty must be one of: arch, product (got 'biz')"));
    }

    #[test]
    fn product_faculty_allowed() {
        let c = fresh();
        let r = courseGet(&json!({ "faculty": "product" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
    }
}
