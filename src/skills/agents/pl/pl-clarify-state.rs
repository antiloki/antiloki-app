//! pl-clarify-state.rs — twin of skills/agents/pl/pl-clarify-state.mjs (db READ lane). NAMES ARE
//! CONTRACT: `plClarifyState` (named default). Human-in-the-loop clarification state for a card.
// QUALITY: 80/100 — PRAGMA column-probe (schema-version tolerant), per-question options + answer
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn plClarifyState(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = match input.get("slug").filter(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false)) {
        Some(v) => v.as_str().unwrap().to_string(), None => return Err("pl-clarify-state: slug is required".into()),
    };
    let empty = || json!({ "pl_session": null, "awaiting_human": false, "pending_count": 0, "questions": [] });

    // The card→session link column varies by migration; probe pl_sessions for whichever exists.
    let cols: Vec<String> = crate::db_json::query_rows(db, "PRAGMA table_info(pl_sessions)", &[])?
        .as_array().unwrap().iter().filter_map(|r| r.get("name").and_then(|v| v.as_str()).map(|s| s.to_string())).collect();
    let link_cols: Vec<&str> = ["approved_card_slug", "card_slug"].into_iter().filter(|c| cols.iter().any(|x| x == c)).collect();
    if link_cols.is_empty() { return Ok(empty()); }

    let where_clause = link_cols.iter().map(|c| format!("{c} = ?")).collect::<Vec<_>>().join(" OR ");
    let params: Vec<Value> = link_cols.iter().map(|_| json!(slug)).collect();
    let session = crate::db_json::query_rows(db, &format!("SELECT id, status FROM pl_sessions WHERE {where_clause} ORDER BY id DESC LIMIT 1"), &params)?;
    let session = session.as_array().and_then(|a| a.first()).cloned();

    let is_clarifying = session.as_ref().and_then(|s| s.get("status")).and_then(|v| v.as_str()) == Some("clarifying");
    if session.is_none() || !is_clarifying {
        return Ok(json!({ "pl_session": session.unwrap_or(Value::Null), "awaiting_human": false, "pending_count": 0, "questions": [] }));
    }
    let session = session.unwrap();
    let sid = session.get("id").cloned().unwrap_or(Value::Null);

    let rows = crate::db_json::query_rows(db,
        "SELECT id, step, question_md, allows_writein, order_index FROM pl_questions WHERE session_id = ? AND step = 'clarify' ORDER BY order_index ASC, id ASC", &[sid.clone()])?;
    let rows = rows.as_array().cloned().unwrap_or_default();

    let mut pending = 0i64;
    let mut questions: Vec<Value> = Vec::new();
    for q in &rows {
        let qid = q.get("id").cloned().unwrap_or(Value::Null);
        let ans = crate::db_json::query_rows(db, "SELECT selected_option_id, writein_md FROM pl_answers WHERE question_id = ?", &[qid.clone()])?;
        let answer = ans.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
        if answer.is_null() { pending += 1; }
        let options = crate::db_json::query_rows(db, "SELECT id, option_md, order_index FROM pl_question_options WHERE question_id = ? ORDER BY order_index ASC, id ASC", &[qid.clone()])?;
        let allows = q.get("allows_writein").map(|v| v.as_i64().map(|i| i != 0).unwrap_or(v.as_bool().unwrap_or(false))).unwrap_or(false);
        questions.push(json!({
            "id": qid, "step": q.get("step").cloned().unwrap_or(Value::Null), "question_md": q.get("question_md").cloned().unwrap_or(Value::Null),
            "allows_writein": allows, "order_index": q.get("order_index").cloned().unwrap_or(Value::Null), "options": options, "answer": answer,
        }));
    }

    Ok(json!({
        "pl_session": { "id": sid, "status": session.get("status").cloned().unwrap_or(Value::Null) },
        "awaiting_human": pending > 0, "pending_count": pending, "questions": questions,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clarify_state() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE pl_sessions (id INTEGER, status TEXT, approved_card_slug TEXT); \
          CREATE TABLE pl_questions (id INTEGER, session_id INTEGER, step TEXT, question_md TEXT, allows_writein INTEGER, order_index INTEGER); \
          CREATE TABLE pl_answers (id INTEGER, question_id INTEGER, selected_option_id INTEGER, writein_md TEXT); \
          CREATE TABLE pl_question_options (id INTEGER, question_id INTEGER, option_md TEXT, order_index INTEGER); \
          INSERT INTO pl_sessions VALUES (1,'clarifying','FEAT-X'); \
          INSERT INTO pl_questions VALUES (10,1,'clarify','Where?',1,0); \
          INSERT INTO pl_question_options VALUES (100,10,'root',0),(101,10,'subdir',1);").unwrap();
        let r = plClarifyState(&json!({ "slug": "FEAT-X" }), &c).unwrap();
        assert_eq!(r["awaiting_human"], json!(true));
        assert_eq!(r["pending_count"], json!(1));
        assert_eq!(r["questions"][0]["options"].as_array().unwrap().len(), 2);
        assert_eq!(plClarifyState(&json!({ "slug": "NOPE" }), &c).unwrap()["awaiting_human"], json!(false));
        assert!(plClarifyState(&json!({}), &c).is_err());
    }
}
