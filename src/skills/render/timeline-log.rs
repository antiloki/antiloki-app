//! timeline-log.rs — twin of skills/render/timeline-log.mjs (db READ lane). NAMES ARE CONTRACT:
//! `timelineLog` (named default). ANSI-colored recent sacred_timeline rows, optional filters.
// QUALITY: 80/100 — dynamic filters, pad(truncate/pad), colorOp on PADDED input (faithful quirk), verbose
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
fn pad(s: &str, width: usize) -> String {
    let cc = s.chars().count();
    if cc >= width { s.chars().take(width).collect() } else { format!("{}{}", s, " ".repeat(width - cc)) }
}

pub fn timelineLog(input: &Value, db: &Connection) -> Result<Value, String> {
    let limit = input.get("limit").and_then(|v| v.as_i64()).unwrap_or(50);
    let color = input.get("color") != Some(&Value::Bool(false));
    let verbose = input.get("verbose") == Some(&Value::Bool(true));
    let sf = |k: &str| input.get(k).filter(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false)).map(|v| v.as_str().unwrap().to_string());
    let (actor_f, etype_f, eid_f, since) = (sf("actor"), sf("entityType"), sf("entityId"), sf("since"));

    let mut sql = String::from("SELECT t.*, c.git_sha FROM sacred_timeline t LEFT JOIN timeline_commits c ON c.timeline_id = t.id WHERE 1=1");
    let mut params: Vec<Value> = Vec::new();
    if let Some(a) = &actor_f { sql.push_str(" AND t.actor = ?"); params.push(json!(a)); }
    if let Some(e) = &etype_f { sql.push_str(" AND t.entity_type = ?"); params.push(json!(e)); }
    if let Some(e) = &eid_f { sql.push_str(" AND t.entity_id = ?"); params.push(json!(e)); }
    if let Some(s) = &since { sql.push_str(" AND t.ts >= ?"); params.push(json!(s)); }
    sql.push_str(" ORDER BY t.id DESC LIMIT ?");
    params.push(json!(limit));
    let mut rows = crate::db_json::query_rows(db, &sql, &params)?.as_array().cloned().unwrap_or_default();
    rows.reverse();

    let mut filters: Vec<String> = Vec::new();
    if let Some(a) = &actor_f { filters.push(format!("actor={a}")); }
    if let Some(e) = &etype_f { filters.push(format!("entity_type={e}")); }
    if let Some(e) = &eid_f { filters.push(format!("entity_id={e}")); }
    if let Some(s) = &since { filters.push(format!("since={s}")); }
    let filter_str = if filters.is_empty() { String::new() } else { format!(" ({})", filters.join(", ")) };

    if rows.is_empty() {
        return Ok(json!(paint("dim", &format!("(no events match{filter_str})"), color)));
    }
    let plural = if rows.len() == 1 { "" } else { "s" };
    let mut lines: Vec<String> = vec![paint("bold", &format!("Timeline · {} event{}{}", rows.len(), plural, filter_str), color), String::new()];
    for r in &rows {
        let ts = paint("dim", &js_str(r.get("ts").unwrap_or(&Value::Null)), color);
        let op = color_op(&pad(&js_str(r.get("operation").unwrap_or(&Value::Null)), 20), color);
        let act = color_actor(&pad(&js_str(r.get("actor").unwrap_or(&Value::Null)), 18), color);
        let ent = paint("bold", &js_str(r.get("entity_id").unwrap_or(&Value::Null)), color);
        let sha = match r.get("git_sha").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            Some(s) => format!("  {}", paint("gray", &s.chars().take(7).collect::<String>(), color)), None => String::new(),
        };
        lines.push(format!("  {}  {}  {}  {}{}", ts, op, act, ent, sha));
        if verbose {
            if let Some(after) = r.get("after_json").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                let indented = after.split('\n').map(|l| paint("dim", &format!("    {l}"), color)).collect::<Vec<_>>().join("\n");
                lines.push(indented);
            }
        }
    }
    Ok(json!(lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seed() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, ts TEXT, operation TEXT, actor TEXT, entity_type TEXT, entity_id TEXT, after_json TEXT); \
          CREATE TABLE timeline_commits (timeline_id INTEGER, git_sha TEXT); \
          INSERT INTO sacred_timeline VALUES (1,'ts1','card.add','human:dani','card','T-0',NULL),(2,'ts2','card.add','agent:pl','card','T-1',NULL);").unwrap();
        c
    }
    #[test]
    fn renders_and_filters() {
        let c = seed();
        let r = timelineLog(&json!({ "color": false }), &c).unwrap();
        assert!(r.as_str().unwrap().contains("Timeline · 2 events"));
        let f = timelineLog(&json!({ "actor": "agent:pl", "color": false }), &c).unwrap();
        assert!(f.as_str().unwrap().contains("Timeline · 1 event ") && f.as_str().unwrap().contains("actor=agent:pl"));
        let empty = timelineLog(&json!({ "actor": "nobody", "color": false }), &c).unwrap();
        assert!(empty.as_str().unwrap().contains("no events match"));
    }
}
