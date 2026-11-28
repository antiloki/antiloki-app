//! EVERY STRUCTURED REPLY GETS A TEMPLATE AND A LINT (operator 2026-09-25, after JEV: "All jsons should have a
//! template and a lint, all this deterministic skills on the [backend]. And for formating low models are better").
//!
//! JEV's guarantee is type-safe output BY CONSTRUCTION: the decoder cannot emit an invalid document. Through a CLI we
//! cannot constrain decoding, so the honest equivalent is the other half of the same idea — declare the shape, check
//! it deterministically, and COUNT how often it is wrong. The counting comes first on purpose: a repair loop for a
//! failure that happens once in fifty runs is code written against a feeling.
//!
//! The templates already declare their shape — `prompts.rs` carries `output: ("tasks", "array_of_object")` on every
//! entry and has since the catalog was written. Nothing read it. This reads it.
//!
//! WHAT A PROBLEM IS, AND WHAT IT IS NOT. A problem is a document that cannot be used: not JSON at all, the declared
//! key missing, the declared key of the wrong kind. It is NOT prose around the JSON (every CLI does that and the
//! parsers already cope), a missing optional field, or an empty array — an empty `problems` list is a ratifier saying
//! "no problems", which is an answer and not a defect.
use serde_json::{json, Value};

/// WHAT THE MODEL SENT, out of whatever it wrapped around it. The same tolerant extraction the parsers already do by
/// hand, in one place: the first balanced `{…}` or `[…]`, preferring whichever opens first. `None` when there is no
/// JSON in the reply at all — which is itself the commonest failure and must be reported as one. Pure.
pub fn extract(text: &str) -> Option<Value> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) { return Some(v) }
    let ob = t.find('{'); let ar = t.find('[');
    let (open, close) = match (ob, ar) {
        (Some(o), Some(a)) if o < a => ('{', '}'),
        (None, Some(_)) => ('[', ']'),
        (Some(_), None) => ('{', '}'),
        (Some(_), Some(_)) => ('[', ']'),
        (None, None) => return None,
    };
    let start = t.find(open)?;
    let end = t.rfind(close)?;
    if end <= start { return None }
    serde_json::from_str::<Value>(&t[start..=end]).ok()
}

/// Does the value match the shape the template declared? `kind` is `prompts::Tpl::output`'s second half. Pure.
pub fn kind_ok(v: &Value, kind: &str) -> bool {
    match kind {
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "number" => v.is_number(),
        "array_of_string" => v.as_array().map(|a| a.iter().all(Value::is_string)).unwrap_or(false),
        "array_of_object" => v.as_array().map(|a| a.iter().all(Value::is_object)).unwrap_or(false),
        "object" => v.is_object(),
        // an undeclared or unknown kind cannot fail a document — the lint's job is to check claims, not to invent them
        _ => true,
    }
}

/// THE LINT. `key`/`kind` are the template's declaration; the reply is whatever the model sent. Returns the problems
/// in the order a person would want to fix them, and an empty list when the document is usable. Pure — tested.
pub fn check(text: &str, key: &str, kind: &str) -> Vec<String> {
    let Some(v) = extract(text) else {
        return vec![if text.trim().is_empty() { "the reply was empty".into() } else { "no JSON in the reply".into() }];
    };
    if key.is_empty() { return vec![] }
    // A BARE DOCUMENT OF THE RIGHT KIND IS FINE. Half the templates are answered with the array itself rather than
    // `{"tasks": [...]}`, every parser here already accepts both, and calling that a defect would count a hundred
    // healthy replies as broken.
    if !v.is_object() { return if kind_ok(&v, kind) { vec![] } else { vec![format!("the reply is not the declared `{kind}`")] } };
    match v.get(key) {
        None => vec![format!("`{key}` is missing")],
        Some(x) if !kind_ok(x, kind) => vec![format!("`{key}` is not {kind}")],
        _ => vec![],
    }
}

fn open() -> Result<rusqlite::Connection, String> {
    let c = crate::merges::open().map_err(|e| e.to_string())?;
    c.execute_batch("CREATE TABLE IF NOT EXISTS json_health (
        base TEXT NOT NULL, engine TEXT NOT NULL DEFAULT '', model TEXT NOT NULL DEFAULT '',
        n INTEGER NOT NULL DEFAULT 0, bad INTEGER NOT NULL DEFAULT 0,
        last_problem TEXT NOT NULL DEFAULT '', last_seen INTEGER NOT NULL DEFAULT 0,
        UNIQUE (base, engine, model));").map_err(|e| e.to_string())?;
    Ok(c)
}

/// COUNT ONE REPLY. Called wherever a structured reply is parsed; cheap enough to leave on forever, which is the point
/// — the rate is the thing we do not have and cannot guess.
pub fn note(base: &str, engine: &str, model: &str, problems: &[String]) {
    let Ok(c) = open() else { return };
    let bad = i64::from(!problems.is_empty());
    let _ = c.execute(
        "INSERT INTO json_health (base, engine, model, n, bad, last_problem, last_seen) VALUES (?1,?2,?3,1,?4,?5,?6)
         ON CONFLICT(base, engine, model) DO UPDATE SET n = n + 1, bad = bad + ?4,
           last_problem = CASE WHEN ?4 = 1 THEN ?5 ELSE last_problem END, last_seen = ?6",
        rusqlite::params![base, engine, model, bad, problems.first().cloned().unwrap_or_default(), crate::cli::now_ms()]);
}

/// the rates, worst first — the answer to "is a repair loop worth building?"
pub fn report() -> Result<Value, String> {
    let c = open()?;
    let mut st = c.prepare("SELECT base, engine, model, n, bad, last_problem, last_seen FROM json_health ORDER BY bad * 1.0 / n DESC, n DESC").map_err(|e| e.to_string())?;
    let rows: Vec<Value> = st.query_map([], |r| {
        let (n, bad): (i64, i64) = (r.get(3)?, r.get(4)?);
        Ok(json!({ "base": r.get::<_, String>(0)?, "engine": r.get::<_, String>(1)?, "model": r.get::<_, String>(2)?,
                   "n": n, "bad": bad, "rate": if n > 0 { (bad as f64 / n as f64 * 1000.0).round() / 1000.0 } else { 0.0 },
                   "last_problem": r.get::<_, String>(5)?, "last_seen": r.get::<_, i64>(6)? }))
    }).map_err(|e| e.to_string())?.filter_map(Result::ok).collect();
    let n: i64 = rows.iter().map(|r| r["n"].as_i64().unwrap_or(0)).sum();
    let bad: i64 = rows.iter().map(|r| r["bad"].as_i64().unwrap_or(0)).sum();
    // THE VERDICT IS ABOUT WHETHER TO BUILD ANYTHING, and it refuses to answer from a handful of replies.
    let verdict = if n < 20 {
        format!("{n} structured replies checked — too few to say whether a repair pass would pay for itself")
    } else if bad == 0 {
        format!("{n} structured replies, none malformed — a repair pass would be code written against a feeling")
    } else {
        format!("{bad} of {n} replies malformed ({:.1}%) — worst: {}", bad as f64 / n as f64 * 100.0,
                rows.first().map(|r| r["base"].as_str().unwrap_or("")).unwrap_or(""))
    };
    Ok(json!({ "ok": true, "rows": rows, "n": n, "bad": bad, "verdict": verdict }))
}

pub async fn http_report() -> axum::response::Response {
    use axum::response::IntoResponse;
    match tokio::task::spawn_blocking(report).await.unwrap_or_else(|e| Err(e.to_string())) {
        Ok(v) => axum::Json(v).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({ "ok": false, "error": e }))).into_response(),
    }
}

/// CHECK A REPLY AGAINST ITS TEMPLATE'S OWN DECLARATION and count it. The one call sites use: they know their base and
/// what answered, and the catalog knows the shape.
pub fn check_base(base: &str, engine: &str, model: &str, text: &str) -> Vec<String> {
    let (key, kind) = crate::prompts::catalog_entry(base).map(|t| t.output).unwrap_or(("", ""));
    let problems = check(text, key, kind);
    note(base, engine, model, &problems);
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_finds_the_document_inside_whatever_the_model_wrapped_around_it() {
        assert_eq!(extract(r#"{"a":1}"#).unwrap()["a"], 1);
        assert_eq!(extract("Sure! Here you go:\n```json\n{\"a\":2}\n```\nhope that helps").unwrap()["a"], 2);
        assert!(extract("[{\"id\":\"t1\"}]").unwrap().is_array());
        assert!(extract("no json here at all").is_none());
        assert!(extract("").is_none());
    }

    #[test]
    fn a_reply_with_no_json_is_the_failure_it_sounds_like() {
        assert_eq!(check("I would suggest refactoring the module.", "tasks", "array_of_object"), vec!["no JSON in the reply"]);
        assert_eq!(check("   ", "tasks", "array_of_object"), vec!["the reply was empty"]);
    }

    #[test]
    fn the_declared_key_and_kind_are_what_is_checked() {
        assert!(check(r#"{"tasks":[{"id":"t1"}]}"#, "tasks", "array_of_object").is_empty());
        assert_eq!(check(r#"{"plan":[]}"#, "tasks", "array_of_object"), vec!["`tasks` is missing"]);
        assert_eq!(check(r#"{"tasks":"soon"}"#, "tasks", "array_of_object"), vec!["`tasks` is not array_of_object"]);
        assert_eq!(check(r#"{"passed":"yes"}"#, "passed", "boolean"), vec!["`passed` is not boolean"]);
        assert!(check(r#"{"passed":false}"#, "passed", "boolean").is_empty());
    }

    #[test]
    fn a_bare_document_of_the_right_kind_is_not_a_defect() {
        // half the templates are answered with the array itself, every parser here accepts both, and calling that
        // malformed would count a hundred healthy replies as broken
        assert!(check(r#"[{"id":"t1"}]"#, "tasks", "array_of_object").is_empty());
        assert_eq!(check(r#"["just","strings"]"#, "tasks", "array_of_object"), vec!["the reply is not the declared `array_of_object`"]);
    }

    #[test]
    fn an_empty_answer_of_the_right_shape_is_an_answer() {
        // a ratifier replying `{"problems": []}` is saying "no problems" — the lint must not read that as a failure
        assert!(check(r#"{"problems":[]}"#, "problems", "array_of_string").is_empty());
        assert!(check(r#"{"tasks":[]}"#, "tasks", "array_of_object").is_empty());
    }

    #[test]
    fn an_undeclared_shape_cannot_fail_a_document() {
        assert!(check(r#"{"anything":1}"#, "", "").is_empty(), "the lint checks claims; it does not invent them");
        assert!(check(r#"{"x":1}"#, "x", "unknown_kind").is_empty());
    }

    #[test]
    fn the_verdict_refuses_to_recommend_work_from_a_handful_of_replies() {
        let _ = std::fs::remove_file(crate::merges::path());
        for _ in 0..5 { note("goal.router_plan", "claude-code", "sonnet", &[]); }
        let r = report().unwrap();
        assert!(r["verdict"].as_str().unwrap().contains("too few"), "{}", r["verdict"]);

        for _ in 0..20 { note("goal.router_plan", "claude-code", "sonnet", &[]); }
        let r2 = report().unwrap();
        assert!(r2["verdict"].as_str().unwrap().contains("none malformed"), "{}", r2["verdict"]);
        assert!(r2["verdict"].as_str().unwrap().contains("against a feeling"));

        note("goal.router_plan", "claude-code", "sonnet", &["`tasks` is missing".to_string()]);
        let r3 = report().unwrap();
        assert_eq!(r3["bad"], 1);
        assert!(r3["verdict"].as_str().unwrap().contains("malformed ("), "{}", r3["verdict"]);
        assert!(r3["rows"][0]["last_problem"].as_str().unwrap().contains("missing"));
    }
}
