//! board-summary.rs — twin of skills/render/board-summary.mjs (db READ lane). NAMES ARE CONTRACT:
//! `boardSummary` (named default). ANSI-colored terminal summary of the board, grouped by status.
// QUALITY: 81/100 — exact ANSI escapes + spacing, STATUS_ORDER sort, byte-identical string assembly
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::BTreeMap;

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
fn color_status(status: &str, enabled: bool) -> String {
    let c = match status { "backlog" => "gray", "doing" => "yellow", "done" => "green", "archived" => "dim", _ => "cyan" };
    paint(c, status, enabled)
}
fn sort_statuses(mut v: Vec<String>) -> Vec<String> {
    let order = ["backlog", "doing", "done", "archived"];
    let idx = |s: &str| order.iter().position(|x| *x == s).map(|i| i as i64).unwrap_or(-1);
    v.sort_by(|a, b| {
        let (ai, bi) = (idx(a), idx(b));
        if ai == -1 && bi == -1 { a.cmp(b) }
        else if ai == -1 { std::cmp::Ordering::Greater }
        else if bi == -1 { std::cmp::Ordering::Less }
        else { ai.cmp(&bi) }
    });
    v
}
fn count(db: &Connection, sql: &str) -> i64 {
    crate::db_json::query_rows(db, sql, &[]).ok()
        .and_then(|r| r.as_array().and_then(|a| a.first()).and_then(|r| r.get("c")).and_then(|v| v.as_i64())).unwrap_or(0)
}

pub fn boardSummary(input: &Value, db: &Connection) -> Result<Value, String> {
    let color = input.get("color") != Some(&Value::Bool(false)); // color !== false

    let cards = crate::db_json::query_rows(db, "SELECT slug, title, status FROM cards ORDER BY slug", &[])?;
    let cards = cards.as_array().cloned().unwrap_or_default();
    let totals_cards = cards.len() as i64;
    let events = count(db, "SELECT COUNT(*) AS c FROM sacred_timeline");
    let commits = count(db, "SELECT COUNT(*) AS c FROM timeline_commits");
    let agents = count(db, "SELECT COUNT(*) AS c FROM agents");
    let actions = count(db, "SELECT COUNT(*) AS c FROM actions");

    // group by status (lists preserve slug order; section order is sort_statuses)
    let mut grouped: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut keys_seen: Vec<String> = Vec::new();
    for card in &cards {
        let status = card.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if !grouped.contains_key(&status) { keys_seen.push(status.clone()); }
        grouped.entry(status).or_default().push(card.clone());
    }

    let mut lines: Vec<String> = vec![paint("bold", "── BOARD ──", color)];
    if cards.is_empty() {
        lines.push(paint("dim", "  (no cards yet)", color));
    } else {
        for status in sort_statuses(keys_seen) {
            let list = &grouped[&status];
            lines.push(format!("{} {}", color_status(&status, color), paint("dim", &format!("({})", list.len()), color)));
            for c in list {
                let slug = c.get("slug").and_then(|v| v.as_str()).unwrap_or("");
                let title = c.get("title").and_then(|v| v.as_str()).unwrap_or("");
                lines.push(format!("  {} {}  {}", paint("gray", "·", color), paint("bold", slug, color), title));
            }
        }
    }
    lines.push(String::new());
    lines.push(paint("bold", "── SYSTEM ──", color));
    lines.push(format!("  {}{}", paint("dim", "cards:   ", color), totals_cards));
    lines.push(format!("  {}{}", paint("dim", "events:  ", color), events));
    lines.push(format!("  {}{}", paint("dim", "commits: ", color), commits));
    lines.push(format!("  {}{}", paint("dim", "agents:  ", color), agents));
    lines.push(format!("  {}{}", paint("dim", "actions:  ", color), actions));

    Ok(json!(lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seed() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE cards (slug TEXT, title TEXT, status TEXT); CREATE TABLE sacred_timeline (x); CREATE TABLE timeline_commits (x); CREATE TABLE agents (x); CREATE TABLE actions (x);").unwrap();
        c
    }
    #[test]
    fn renders_plain() {
        let c = seed();
        let r = boardSummary(&json!({ "color": false }), &c).unwrap();
        let s = r.as_str().unwrap();
        assert!(s.contains("── BOARD ──"));
        assert!(s.contains("(no cards yet)"));
        assert!(s.contains("cards:   0"));
        c.execute_batch("INSERT INTO cards VALUES ('B-1','one','backlog'),('D-1','d','doing');").unwrap();
        let r2 = boardSummary(&json!({ "color": false }), &c).unwrap();
        assert!(r2.as_str().unwrap().contains("backlog (1)"));
        assert!(r2.as_str().unwrap().contains("doing (1)"));
    }
}
