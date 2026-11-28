//! log-tail-ansi.rs — twin of skills/render/log-tail-ansi.mjs (db READ lane). NAMES ARE CONTRACT:
//! `logTailAnsi` (named default). ANSI table of the last N runtime_events, for a sibling terminal.
// QUALITY: 79/100 — padStart/padEnd (JS-exact), toFixed(2) duration, colorOutcome padEnd over ANSI
use rusqlite::Connection;
use serde_json::{json, Value};

const RESET: &str = "\x1b[0m";
fn c(name: &str) -> &'static str {
    match name {
        "dim" => "\x1b[2m", "reset" => "\x1b[0m", "cyan" => "\x1b[36m", "green" => "\x1b[32m",
        "yellow" => "\x1b[33m", "red" => "\x1b[31m", "blue" => "\x1b[34m", "magenta" => "\x1b[35m",
        "bold" => "\x1b[1m", "gray" => "\x1b[90m", _ => "",
    }
}
fn pad_start(s: &str, n: usize) -> String { let l = s.chars().count(); if l >= n { s.to_string() } else { format!("{}{}", " ".repeat(n - l), s) } }
fn pad_end(s: &str, n: usize) -> String { let l = s.chars().count(); if l >= n { s.to_string() } else { format!("{}{}", s, " ".repeat(n - l)) } }
fn slice(s: &str, start: usize, end: usize) -> String { s.chars().skip(start).take(end.saturating_sub(start)).collect() }

fn color_outcome(o: &str) -> String {
    match o {
        "success" => format!("{}{}{}", c("green"), o, RESET),
        "error" | "blocked" => format!("{}{}{}", c("red"), o, RESET),
        "in_flight" => format!("{}{}{}", c("yellow"), o, RESET),
        _ => format!("{}{}{}", c("gray"), o, RESET),
    }
}
fn color_duration(ms: Option<f64>) -> String {
    match ms {
        None => format!("{}   —   {}", c("gray"), RESET),
        Some(ms) => {
            let str_ = pad_start(&format!("{ms:.2}"), 7);
            let col = if ms > 1000.0 { "red" } else if ms > 100.0 { "yellow" } else { "gray" };
            format!("{}{}ms{}", c(col), str_, RESET)
        }
    }
}

pub fn logTailAnsi(input: &Value, db: &Connection) -> Result<Value, String> {
    let limit = input.get("limit").and_then(|v| v.as_i64()).unwrap_or(30);
    let sf = |k: &str| input.get(k).filter(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false)).map(|v| v.as_str().unwrap().to_string());
    let since_id = input.get("since_id").filter(|v| !v.is_null());
    let skill_filter = sf("skill_filter");
    let outcome = sf("outcome");

    let mut wh: Vec<&str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(s) = since_id { wh.push("id > ?"); params.push(s.clone()); }
    if let Some(sk) = &skill_filter { wh.push("action LIKE ?"); params.push(json!(format!("%{sk}%"))); }
    if let Some(o) = &outcome { wh.push("outcome = ?"); params.push(json!(o)); }
    let where_clause = if wh.is_empty() { String::new() } else { format!("WHERE {}", wh.join(" AND ")) };
    let sql = format!("SELECT id, ts, parent_event_id, actor, agent, action, skill, duration_ms, outcome, error_code FROM runtime_events {where_clause} ORDER BY id DESC LIMIT ?");
    params.push(json!(limit.clamp(1, 500)));
    let mut rows = crate::db_json::query_rows(db, &sql, &params)?.as_array().cloned().unwrap_or_default();
    rows.reverse();

    if rows.is_empty() {
        return Ok(json!(format!("{}runtime_events: empty (no actions dispatched yet){}", c("gray"), RESET)));
    }
    let mut lines: Vec<String> = vec![
        format!("{}runtime_events · last {}{}", c("bold"), rows.len(), RESET),
        String::new(),
        format!("{}{}  {}  {}  {}  {}  what{}", c("dim"), pad_start("id", 5), pad_end("ts", 12), pad_start("dur", 9), pad_end("outcome", 10), pad_end("agent", 12), RESET),
    ];
    for r in &rows {
        let g = |k: &str| r.get(k).cloned().unwrap_or(Value::Null);
        let ts = pad_end(&slice(g("ts").as_str().unwrap_or(""), 11, 23), 12);
        let what = if g("action").as_str().map(|s| !s.is_empty()).unwrap_or(false) {
            format!("{}action={}{}", c("cyan"), g("action").as_str().unwrap(), RESET)
        } else if g("skill").as_str().map(|s| !s.is_empty()).unwrap_or(false) {
            format!("{}sub={}{}", c("blue"), g("skill").as_str().unwrap(), RESET)
        } else { "?".to_string() };
        let pe = g("parent_event_id");
        let parent = if pe.is_null() || pe.as_i64() == Some(0) { String::new() } else { format!("{} ←{}{}", c("gray"), jnum(&pe), RESET) };
        let errs = match g("error_code").as_str().filter(|s| !s.is_empty()) { Some(e) => format!("{} [{}]{}", c("red"), e, RESET), None => String::new() };
        let dur = color_duration(g("duration_ms").as_f64());
        let outcome_s = pad_end(&color_outcome(g("outcome").as_str().unwrap_or("")), 20);
        let agent = pad_end(&(g("agent").as_str().map(|s| s.to_string()).unwrap_or_else(|| "-".to_string())), 12);
        lines.push(format!("{}{}{}  {}{}{}  {}  {}  {}  {}{}{}", c("gray"), pad_start(&g("id").as_i64().map(|i| i.to_string()).unwrap_or_default(), 5), RESET, c("gray"), ts, RESET, dur, outcome_s, agent, what, parent, errs));
    }
    Ok(json!(lines.join("\n")))
}

fn jnum(v: &Value) -> String { if let Some(i) = v.as_i64() { i.to_string() } else { v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string()) } }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders_table() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE runtime_events (id INTEGER PRIMARY KEY, ts TEXT, parent_event_id INTEGER, actor TEXT, agent TEXT, action TEXT, skill TEXT, duration_ms INTEGER, outcome TEXT, error_code TEXT); \
          INSERT INTO runtime_events VALUES (1,'2026-01-01 12:00:00.000',NULL,'a','pl','card.add','card-add',50,'success',NULL);").unwrap();
        let r = logTailAnsi(&json!({ "color": false }), &c).unwrap();
        assert!(r.as_str().unwrap().contains("runtime_events · last 1"));
        assert!(r.as_str().unwrap().contains("action=card.add"));
        let empty = logTailAnsi(&json!({ "outcome": "nope" }), &c).unwrap();
        assert!(empty.as_str().unwrap().contains("empty (no actions"));
    }
}
