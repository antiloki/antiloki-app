//! pl-questionnaire-md.rs — twin of skills/render/pl-questionnaire-md.mjs (db READ lane). NAMES ARE
//! CONTRACT: `plQuestionnaireMd` (named default). Render pending pl_questions as fillable markdown.
// QUALITY: 79/100 — number-or-object id, toFixed(4) budget, per-step/question/option markdown assembly
use rusqlite::Connection;
use serde_json::{json, Value};

fn js_str(v: &Value) -> String { match v { Value::String(s) => s.clone(), Value::Null => "null".into(), o => o.to_string() } }
fn jnum(v: &Value) -> i64 { v.as_i64().unwrap_or(0) }

pub fn plQuestionnaireMd(input: &Value, db: &Connection) -> Result<Value, String> {
    let session_id = if input.is_number() { input.clone() } else { input.get("session_id").cloned().unwrap_or(Value::Null) };
    if session_id.is_null() || session_id.as_i64() == Some(0) {
        return Err("pl-questionnaire-md: session_id is required".into());
    }
    let sid_str = js_str(&session_id);

    let session = crate::db_json::query_rows(db, "SELECT * FROM pl_sessions WHERE id = ?", &[session_id.clone()])?;
    let session = match session.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("pl-questionnaire-md: session {sid_str} not found")),
    };
    let sg = |k: &str| session.get(k).cloned().unwrap_or(Value::Null);

    let questions = crate::db_json::query_rows(db,
        "SELECT q.id, q.question_md, q.allows_writein, q.step, q.order_index, a.id AS answer_id, a.selected_option_id, a.writein_md AS answer_writein \
         FROM pl_questions q LEFT JOIN pl_answers a ON a.question_id = q.id WHERE q.session_id = ? ORDER BY q.step, q.order_index",
        &[session_id.clone()])?;
    let questions = questions.as_array().cloned().unwrap_or_default();

    let mut lines: Vec<String> = vec![
        format!("# PL session #{sid_str}"), String::new(),
        format!("> **Intent:** {}", js_str(&sg("raw_intent"))),
        format!("> **Status:** `{}` · **Started by:** `{}`", js_str(&sg("status")), js_str(&sg("started_by"))),
        format!("> **Budget:** ${:.4} · **Spent:** ${:.4}", sg("budget_usd").as_f64().unwrap_or(0.0), sg("spent_usd").as_f64().unwrap_or(0.0)),
        String::new(),
    ];

    if questions.is_empty() {
        lines.push("_No questions yet. Run `pl.session.advance.clarify` first._".into());
        return Ok(json!(lines.join("\n")));
    }
    let answered = questions.iter().filter(|q| !q.get("answer_id").map(|v| v.is_null()).unwrap_or(true)).count();
    let pending = questions.len() - answered;
    lines.push(format!("**Progress:** {answered} answered, {pending} pending."));
    lines.push(String::new());

    let mut current_step: Option<Value> = None;
    for q in &questions {
        let qg = |k: &str| q.get(k).cloned().unwrap_or(Value::Null);
        let step = qg("step");
        if current_step.as_ref() != Some(&step) {
            current_step = Some(step.clone());
            lines.push("---".into());
            lines.push(format!("## Step: {}", js_str(&step)));
            lines.push(String::new());
        }
        let has_answer = !qg("answer_id").is_null();
        let status = if has_answer { "✓ answered" } else { "○ pending" };
        lines.push(format!("### Q{}: {}  _( {} )_", jnum(&qg("order_index")) + 1, js_str(&qg("question_md")), status));
        let options = crate::db_json::query_rows(db, "SELECT id, option_md, order_index FROM pl_question_options WHERE question_id = ? ORDER BY order_index", &[qg("id")])?;
        for opt in options.as_array().unwrap() {
            let checked = if qg("selected_option_id") == opt.get("id").cloned().unwrap_or(Value::Null) { "x" } else { " " };
            lines.push(format!("- [{}] **{}**  _(id={})_", checked, js_str(opt.get("option_md").unwrap_or(&Value::Null)), js_str(opt.get("id").unwrap_or(&Value::Null))));
        }
        if qg("allows_writein").as_i64().map(|i| i != 0).unwrap_or(qg("allows_writein").as_bool().unwrap_or(false)) {
            let w = if !qg("answer_writein").is_null() { format!("**{}**", js_str(&qg("answer_writein"))) } else { "_______________________________".to_string() };
            lines.push(format!("- [ ] **Write-in:** {w}"));
        }
        if !has_answer {
            let qid = js_str(&qg("id"));
            lines.push(String::new());
            lines.push(format!("  **Submit:** `node bin/antiloki.mjs pl.session.answer.record '{{\"question_id\":{qid},\"selected_option_id\":<id>}}'`"));
            lines.push(format!("  **Or:** `node bin/antiloki.mjs pl.session.answer.record '{{\"question_id\":{qid},\"writein_md\":\"your answer\"}}'`"));
        }
        lines.push(String::new());
    }
    Ok(json!(lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders_questionnaire() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE pl_sessions (id INTEGER, raw_intent TEXT, status TEXT, started_by TEXT, budget_usd REAL, spent_usd REAL); \
          CREATE TABLE pl_questions (id INTEGER, session_id INTEGER, question_md TEXT, allows_writein INTEGER, step TEXT, order_index INTEGER); \
          CREATE TABLE pl_answers (id INTEGER, question_id INTEGER, selected_option_id INTEGER, writein_md TEXT); \
          CREATE TABLE pl_question_options (id INTEGER, question_id INTEGER, option_md TEXT, order_index INTEGER); \
          INSERT INTO pl_sessions VALUES (1,'build a thing','clarify','human:dani',5.0,1.25); \
          INSERT INTO pl_questions VALUES (10,1,'Which db?',1,'scope',0); \
          INSERT INTO pl_question_options VALUES (100,10,'sqlite',0),(101,10,'postgres',1);").unwrap();
        let r = plQuestionnaireMd(&json!({ "session_id": 1 }), &c).unwrap();
        let s = r.as_str().unwrap();
        assert!(s.contains("# PL session #1"));
        assert!(s.contains("$5.0000") && s.contains("$1.2500"));
        assert!(s.contains("### Q1: Which db?"));
        assert!(s.contains("**sqlite**"));
        assert!(plQuestionnaireMd(&json!({ "session_id": 99 }), &c).is_err());
    }
}
