//! mcp-render-sessions.rs — twin of skills/external/mcp-render-sessions.mjs (db READ lane).
//! NAMES ARE CONTRACT: `mcpRenderSessions` (default). Renders recent interviewer sessions +
//! their latest turns as markdown, wrapped via the already-converted `toMcpMarkdown` twin.
// QUALITY: schema-aware ts column (created_at | ts), pipe-escape + 80-char slice on user_text
use rusqlite::Connection;
use serde_json::{json, Value};

// JS `${v}` interpolation for the values that appear (string/number/absent/null).
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS truthiness (used for `t.confirmed ? '✓' : ''`).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

pub fn mcpRenderSessions(input: &Value, db: &Connection) -> Result<Value, String> {
    // `const { limit = 10, turns_per_session = 3 } = input ?? {}` — default only on missing key.
    let limit = match input.get("limit") { None => json!(10), Some(v) => v.clone() };
    let turns_per_session = match input.get("turns_per_session") { None => json!(3), Some(v) => v.clone() };

    let sessions = crate::db_json::query_rows(
        db,
        "SELECT s.id, s.started_at, s.user_identifier, s.status, \
                (SELECT COUNT(*) FROM interviewer_turns t WHERE t.session_id = s.id) AS turn_count \
         FROM interviewer_sessions s ORDER BY s.id DESC LIMIT ?",
        &[limit.clone()],
    )?
    .as_array().cloned().unwrap_or_default();

    // Schema-aware: live DB uses created_at, :memory: open-db uses ts.
    let turn_cols = crate::db_json::query_rows(db, "PRAGMA table_info(interviewer_turns)", &[])?
        .as_array().cloned().unwrap_or_default();
    let has_created_at = turn_cols.iter().any(|c| c.get("name").and_then(Value::as_str) == Some("created_at"));
    let ts_col = if has_created_at { "created_at" } else { "ts" };

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("# interviewer sessions (last {} of {} requested)", sessions.len(), tmpl(Some(&limit))));
    lines.push(String::new());

    if sessions.is_empty() {
        lines.push("_no sessions yet \u{2014} open the FE chat or interviewer REPL to start one_".into());
        let payload = json!({ "markdown": lines.join("\n"), "data": { "sessions": [] } });
        return crate::external_to_mcp_markdown::toMcpMarkdown(&payload, &json!({}));
    }

    let turns_sql = format!(
        "SELECT id, user_text, classified_kind, confirmed, {} AS ts \
         FROM interviewer_turns WHERE session_id = ? ORDER BY id DESC LIMIT ?",
        ts_col
    );

    for s in &sessions {
        lines.push(format!(
            "## session #{} \u{00b7} {} \u{00b7} {} turns",
            tmpl(s.get("id")), tmpl(s.get("status")), tmpl(s.get("turn_count"))
        ));
        lines.push(format!("started {} by `{}`", tmpl(s.get("started_at")), tmpl(s.get("user_identifier"))));
        lines.push(String::new());

        let sid = s.get("id").cloned().unwrap_or(Value::Null);
        let turns = crate::db_json::query_rows(db, &turns_sql, &[sid, turns_per_session.clone()])?
            .as_array().cloned().unwrap_or_default();

        if turns.is_empty() {
            lines.push("_(no turns yet)_".into());
        } else {
            lines.push("| turn | kind | confirmed | user said |".into());
            lines.push("|---|---|---|---|".into());
            for t in &turns {
                let check = if truthy(t.get("confirmed")) { "\u{2713}" } else { "" };
                let raw = match nz(t.get("user_text")) { Some(Value::String(x)) => x.clone(), _ => String::new() };
                let said = raw.chars().take(80).collect::<String>().replace('|', "\\|");
                lines.push(format!("| {} | `{}` | {} | {} |", tmpl(t.get("id")), tmpl(t.get("classified_kind")), check, said));
            }
        }
        lines.push(String::new());
    }
    lines.push(r"_call \`mcp.reasoning\` with a `turn_id` to see full per-turn telemetry._".into());

    let payload = json!({ "markdown": lines.join("\n"), "data": { "sessions": sessions } });
    crate::external_to_mcp_markdown::toMcpMarkdown(&payload, &json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mkdb(ts_created: bool) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE interviewer_sessions (id INTEGER PRIMARY KEY, started_at TEXT, user_identifier TEXT, status TEXT);").unwrap();
        let ts = if ts_created { "created_at" } else { "ts" };
        c.execute_batch(&format!("CREATE TABLE interviewer_turns (id INTEGER PRIMARY KEY, session_id INTEGER, user_text TEXT, classified_kind TEXT, confirmed INTEGER, {} TEXT);", ts)).unwrap();
        c
    }

    #[test]
    fn empty_sessions() {
        let c = mkdb(false);
        let r = mcpRenderSessions(&json!({ "limit": 5 }), &c).unwrap();
        assert_eq!(r["content"][0]["text"], json!("# interviewer sessions (last 0 of 5 requested)\n\n_no sessions yet \u{2014} open the FE chat or interviewer REPL to start one_"));
        assert_eq!(r["_meta"]["antiloki"]["data"]["sessions"], json!([]));
    }

    #[test]
    fn sessions_with_turns() {
        let c = mkdb(false);
        c.execute_batch("INSERT INTO interviewer_sessions (id, started_at, user_identifier, status) VALUES \
            (1,'2026-01-01T00:00:00Z','dani','open'),(2,'2026-01-02T00:00:00Z','claude','closed');").unwrap();
        c.execute_batch("INSERT INTO interviewer_turns (id, session_id, user_text, classified_kind, confirmed, ts) VALUES \
            (10,1,'hello | world with a pipe','question',1,'t1'),(11,1,'second turn text','command',0,'t2'),(12,2,NULL,'note',0,'t3');").unwrap();
        let r = mcpRenderSessions(&json!({ "limit": 10, "turns_per_session": 2 }), &c).unwrap();
        let text = r["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("# interviewer sessions (last 2 of 10 requested)\n\n## session #2 \u{00b7} closed \u{00b7} 1 turns"));
        assert!(text.contains("| 10 | `question` | \u{2713} | hello \\| world with a pipe |"));
        assert!(text.contains("| 12 | `note` |  |  |"));
        assert!(text.ends_with(r"_call \`mcp.reasoning\` with a `turn_id` to see full per-turn telemetry._"));
        assert_eq!(r["_meta"]["antiloki"]["data"]["sessions"][0]["id"], json!(2));
        assert_eq!(r["_meta"]["antiloki"]["data"]["sessions"][0]["turn_count"], json!(1));
    }

    #[test]
    fn no_turns_and_slice() {
        let c = mkdb(false);
        c.execute_batch("INSERT INTO interviewer_sessions (id, started_at, user_identifier, status) VALUES \
            (5,'2026-03-03','u5','open'),(6,'2026-03-04','u6','closed');").unwrap();
        let long = format!("P|{}", "x".repeat(100));
        c.execute("INSERT INTO interviewer_turns (id, session_id, user_text, classified_kind, confirmed, ts) VALUES (50,5,?1,'longkind',1,'t5')", [long]).unwrap();
        let r = mcpRenderSessions(&json!({}), &c).unwrap();
        let text = r["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("## session #6 \u{00b7} closed \u{00b7} 0 turns\nstarted 2026-03-04 by `u6`\n\n_(no turns yet)_"));
        assert!(text.contains(&format!("| 50 | `longkind` | \u{2713} | P\\|{} |", "x".repeat(78))));
    }

    #[test]
    fn created_at_variant() {
        let c = mkdb(true);
        c.execute_batch("INSERT INTO interviewer_sessions (id, started_at, user_identifier, status) VALUES (3,'2026-02-02','x','open');").unwrap();
        c.execute_batch("INSERT INTO interviewer_turns (id, session_id, user_text, classified_kind, confirmed, created_at) VALUES (20,3,'ct','k',1,'c1');").unwrap();
        let r = mcpRenderSessions(&json!({}), &c).unwrap();
        let text = r["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("## session #3 \u{00b7} open \u{00b7} 1 turns"));
        assert!(text.contains("| 20 | `k` | \u{2713} | ct |"));
    }
}
