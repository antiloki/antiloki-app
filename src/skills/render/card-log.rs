//! card-log.rs — twin of skills/render/card-log.mjs (db READ lane). NAMES ARE CONTRACT:
//! `cardLog` (named default). ANSI-colored chronological audit log for one card.
// QUALITY: 81/100 — string-or-object slug, op/actor/status coloring, sacred_timeline JOIN, byte-exact
use rusqlite::Connection;
use serde_json::{json, Value};

fn ansi(color: &str) -> &'static str {
    match color {
        "reset" => "\x1b[0m", "bold" => "\x1b[1m", "dim" => "\x1b[2m",
        "red" => "\x1b[31m", "green" => "\x1b[32m", "yellow" => "\x1b[33m",
        "blue" => "\x1b[34m", "magenta" => "\x1b[35m", "cyan" => "\x1b[36m",
        "gray" => "\x1b[90m", "brightBlue" => "\x1b[94m", _ => "",
    }
}
fn paint(color: &str, text: &str, enabled: bool) -> String {
    if !enabled { return text.to_string(); }
    format!("{}{}{}", ansi(color), text, ansi("reset"))
}
fn js_str(v: &Value) -> String { match v { Value::String(s) => s.clone(), Value::Null => "null".into(), o => o.to_string() } }

fn color_op(op: &str, e: bool) -> String {
    if op.ends_with(".add") { paint("green", op, e) }
    else if op.ends_with(".update") || op.ends_with(".change") { paint("yellow", op, e) }
    else if op.ends_with(".archive") || op.ends_with(".delete") { paint("red", op, e) }
    else { paint("cyan", op, e) }
}
fn color_actor(actor: &str, e: bool) -> String {
    if actor.starts_with("human:") { paint("brightBlue", actor, e) }
    else if actor.starts_with("agent:") { paint("magenta", actor, e) }
    else if actor.starts_with("mcp:") { paint("cyan", actor, e) }
    else if actor == "system" { paint("gray", actor, e) }
    else { paint("dim", actor, e) }
}
fn color_status(status: &str, e: bool) -> String {
    let c = match status { "backlog" => "gray", "doing" => "yellow", "done" => "green", "archived" => "dim", _ => "cyan" };
    paint(c, status, e)
}

pub fn cardLog(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug_val = if input.is_string() { input.clone() } else { input.get("slug").cloned().unwrap_or(Value::Null) };
    let slug = match slug_val.as_str().filter(|s| !s.is_empty()) { Some(s) => s.to_string(), None => return Err("card-log: slug is required".into()) };
    let color = if input.is_string() { true } else { input.get("color") != Some(&Value::Bool(false)) };

    let card = crate::db_json::query_rows(db, "SELECT * FROM cards WHERE slug = ?", &[json!(slug)])?;
    let card = match card.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Ok(json!(paint("red", &format!("card '{slug}' not found"), color))),
    };
    let g = |k: &str| card.get(k).cloned().unwrap_or(Value::Null);

    let events = crate::db_json::query_rows(db,
        "SELECT t.*, c.git_sha FROM sacred_timeline t LEFT JOIN timeline_commits c ON c.timeline_id = t.id \
         WHERE t.entity_type = 'card' AND t.entity_id = ? ORDER BY t.id ASC", &[json!(slug)])?;
    let events = events.as_array().cloned().unwrap_or_default();

    let mut lines: Vec<String> = Vec::new();
    lines.push(paint("bold", &format!("Card · {slug}"), color));
    lines.push(format!("  {}{}", paint("dim", "title:  ", color), js_str(&g("title"))));
    lines.push(format!("  {}{}", paint("dim", "status: ", color), color_status(g("status").as_str().unwrap_or(""), color)));
    lines.push(format!("  {} {}", paint("dim", "created:", color), js_str(&g("created_at"))));
    lines.push(format!("  {} {}", paint("dim", "updated:", color), js_str(&g("updated_at"))));
    if g("body_json").as_str().map(|s| !s.is_empty()).unwrap_or(false) {
        lines.push(format!("  {}{}", paint("dim", "body:   ", color), js_str(&g("body_json"))));
    }
    lines.push(String::new());
    lines.push(paint("bold", &format!("Events · {}", events.len()), color));
    if events.is_empty() {
        lines.push(paint("dim", "  (none)", color));
    } else {
        for e in &events {
            let sha = match e.get("git_sha").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                Some(s) => paint("gray", &format!("{} ", s.chars().take(7).collect::<String>()), color),
                None => "         ".to_string(),
            };
            let ts = paint("dim", &js_str(&e.get("ts").cloned().unwrap_or(Value::Null)), color);
            lines.push(format!("  {}  {} {}  by {}", ts, sha, color_op(e.get("operation").and_then(|v| v.as_str()).unwrap_or(""), color), color_actor(e.get("actor").and_then(|v| v.as_str()).unwrap_or(""), color)));
        }
    }
    Ok(json!(lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders_and_not_found() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE cards (slug TEXT, title TEXT, status TEXT, created_at TEXT, updated_at TEXT, body_json TEXT); \
          CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, entity_type TEXT, entity_id TEXT, operation TEXT, actor TEXT, ts TEXT); \
          CREATE TABLE timeline_commits (timeline_id INTEGER, git_sha TEXT); \
          INSERT INTO cards VALUES ('CL-1','My card','backlog','t1','t2',NULL); \
          INSERT INTO sacred_timeline VALUES (1,'card','CL-1','card.add','human:dani','ts1');").unwrap();
        let r = cardLog(&json!({ "slug": "CL-1", "color": false }), &c).unwrap();
        let s = r.as_str().unwrap();
        assert!(s.contains("Card · CL-1"));
        assert!(s.contains("Events · 1"));
        assert!(s.contains("card.add") && s.contains("human:dani"));
        assert!(cardLog(&json!({ "slug": "GHOST", "color": false }), &c).unwrap().as_str().unwrap().contains("card 'GHOST' not found"));
        assert!(cardLog(&json!("CL-1"), &c).unwrap().as_str().unwrap().contains("Card · CL-1")); // string input
    }
}
