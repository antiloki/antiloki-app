//! memory-log.rs — twin of skills/render/memory-log.mjs (db READ lane, cross-db: memory.memories).
//! NAMES ARE CONTRACT: `memoryLog` (default). Colored text feed of memories, grouped by type.
// QUALITY: 80/100 — ANSI paint helpers, type-color map w/ 'cyan' fallback, grouped+ordered render

use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }

const ANSI_RESET: &str = "\x1b[0m";
fn ansi(name: &str) -> &'static str {
    match name {
        "reset" => "\x1b[0m", "bold" => "\x1b[1m", "dim" => "\x1b[2m",
        "red" => "\x1b[31m", "green" => "\x1b[32m", "yellow" => "\x1b[33m",
        "blue" => "\x1b[34m", "magenta" => "\x1b[35m", "cyan" => "\x1b[36m",
        "gray" => "\x1b[90m", "brightBlue" => "\x1b[94m",
        _ => "",
    }
}
fn paint(color: &str, text: &str, enabled: bool) -> String {
    if !enabled { return text.to_string(); }
    format!("{}{}{}", ansi(color), text, ANSI_RESET)
}
fn type_color(t: &str) -> &'static str {
    match t {
        "lesson" => "green", "preference" => "brightBlue", "decision" => "yellow",
        "reference" => "cyan", "correction" => "red", _ => "cyan",
    }
}
fn color_type(t: &str, enabled: bool) -> String { paint(type_color(t), t, enabled) }

pub fn memoryLog(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('memory-log','db') when no db — the db lane always injects one.
    let color = !matches!(input.get("color"), Some(Value::Bool(false)));
    let limit = input.get("limit").filter(|v| !v.is_null()).cloned().unwrap_or(json!(100));
    let type_filter = input.get("type").filter(|v| !v.is_null()).cloned();
    let validated_filter = input.get("validated").filter(|v| !v.is_null()).cloned();

    let mut sql = String::from("SELECT * FROM memory.memories WHERE superseded_by IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if truthy(type_filter.as_ref()) {
        sql.push_str(" AND type = ?");
        params.push(type_filter.clone().unwrap());
    }
    if validated_filter == Some(Value::Bool(true)) { sql.push_str(" AND validated = 1"); }
    if validated_filter == Some(Value::Bool(false)) { sql.push_str(" AND validated = 0"); }
    sql.push_str(" ORDER BY type, id DESC LIMIT ?");
    params.push(limit);

    let rows = crate::db_json::query_rows(db, &sql, &params)?.as_array().cloned().unwrap_or_default();

    if rows.is_empty() {
        return Ok(json!(paint("dim", "(no memories match)", color)));
    }

    let mut order: Vec<String> = Vec::new();
    let mut grouped: std::collections::HashMap<String, Vec<Value>> = std::collections::HashMap::new();
    for r in &rows {
        let t = r.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if !grouped.contains_key(&t) { order.push(t.clone()); grouped.insert(t.clone(), Vec::new()); }
        grouped.get_mut(&t).unwrap().push(r.clone());
    }

    let mut lines: Vec<String> = vec![
        paint("bold", &format!("Memory \u{b7} {} entr{}", rows.len(), if rows.len() == 1 { "y" } else { "ies" }), color),
        String::new(),
    ];
    for t in &order {
        let group = &grouped[t];
        lines.push(format!("{} {}", color_type(t, color), paint("dim", &format!("({})", group.len()), color)));
        for m in group {
            let valid_badge = if truthy(m.get("validated")) { paint("green", "\u{2713}", color) } else { paint("dim", "\u{b7}", color) };
            let conf = paint("dim", &format!("[{:.2}]", m.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.0)), color);
            let id_str = paint("gray", &format!("#{}", tmpl(m.get("id"))), color);
            lines.push(format!("  {} {} {} {}", valid_badge, id_str, conf, paint("bold", &tmpl(m.get("title")), color)));
            lines.push(format!("     {}{}", paint("dim", "when: ", color), tmpl(m.get("situation"))));
            lines.push(format!("     {}{}", paint("dim", "do:   ", color), tmpl(m.get("guidance"))));
            if truthy(m.get("rationale")) {
                lines.push(format!("     {}{}", paint("dim", "why:  ", color), tmpl(m.get("rationale"))));
            }
            if truthy(m.get("naive_response")) {
                lines.push(format!("     {} {}", paint("red", "naive:", color), paint("dim", &tmpl(m.get("naive_response")), color)));
            }
        }
        lines.push(String::new());
    }
    Ok(json!(lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory;
            CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL, situation TEXT NOT NULL, guidance TEXT NOT NULL, rationale TEXT, naive_response TEXT, confidence REAL NOT NULL DEFAULT 0.5, validated INTEGER NOT NULL DEFAULT 0, superseded_by INTEGER, created_by TEXT NOT NULL);
            INSERT INTO memory.memories (id,type,title,situation,guidance,rationale,naive_response,confidence,validated,superseded_by,created_by) VALUES
              (1,'lesson','L-old','sit1','gd1',NULL,NULL,0.4,0,NULL,'test'),
              (2,'lesson','L-new','sit2','gd2','because reasons',NULL,0.83,1,NULL,'test'),
              (3,'preference','Pref1','sit3','gd3',NULL,NULL,1,1,NULL,'test'),
              (4,'decision','Dec1','sit4','gd4',NULL,NULL,0.5,0,NULL,'test'),
              (5,'correction','Corr1','sit5','gd5','why it happened','wrong guess here',0.75,0,NULL,'test'),
              (6,'reference','Ref1','sit6','gd6',NULL,NULL,0.6,1,NULL,'test'),
              (7,'narrative','Narr1','sit7','gd7',NULL,NULL,0.33,0,NULL,'test'),
              (8,'lesson','L-superseded','sit8','gd8',NULL,NULL,0.2,0,2,'test');").unwrap();
        c
    }

    #[test]
    fn empty_returns_no_memories_message() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory; CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, type TEXT NOT NULL, title TEXT NOT NULL, situation TEXT NOT NULL, guidance TEXT NOT NULL, rationale TEXT, naive_response TEXT, confidence REAL NOT NULL DEFAULT 0.5, validated INTEGER NOT NULL DEFAULT 0, superseded_by INTEGER, created_by TEXT NOT NULL);").unwrap();
        let r = memoryLog(&json!({ "color": false }), &c).unwrap();
        assert_eq!(r, json!("(no memories match)"));
    }

    #[test]
    fn groups_by_type_excludes_superseded_and_pluralizes() {
        let r = memoryLog(&json!({ "color": false }), &seeded()).unwrap();
        let s = r.as_str().unwrap();
        assert!(s.contains("Memory \u{b7} 7 entries")); // id 8 (superseded_by=2) excluded
        assert!(s.contains("correction (1)"));
        assert!(s.contains("lesson (2)"));
        assert!(!s.contains("L-superseded"));
        assert!(!s.contains('\x1b')); // color:false → no ANSI
    }

    #[test]
    fn color_true_emits_ansi_and_null_does_not_disable() {
        let with_default = memoryLog(&json!({}), &seeded()).unwrap();
        assert!(with_default.as_str().unwrap().contains('\x1b'));
        let with_null = memoryLog(&json!({ "color": null }), &seeded()).unwrap();
        assert!(with_null.as_str().unwrap().contains('\x1b')); // null !== false, JS keeps color on
    }

    #[test]
    fn rationale_and_naive_response_branches() {
        let r = memoryLog(&json!({ "type": "correction", "color": false }), &seeded()).unwrap();
        let s = r.as_str().unwrap();
        assert!(s.contains("why:  why it happened"));
        assert!(s.contains("naive: wrong guess here"));
        assert!(s.contains("[0.75]"));
        let l = memoryLog(&json!({ "type": "lesson", "color": false }), &seeded()).unwrap();
        assert!(l.as_str().unwrap().contains("Memory \u{b7} 2 entries"));
    }

    #[test]
    fn validated_filter_and_narrative_fallback_color() {
        let v_true = memoryLog(&json!({ "validated": true, "color": false }), &seeded()).unwrap();
        let s = v_true.as_str().unwrap();
        assert!(s.contains("Pref1") && s.contains("Ref1") && !s.contains("Dec1"));
        let v_false = memoryLog(&json!({ "validated": false, "color": false }), &seeded()).unwrap();
        assert!(v_false.as_str().unwrap().contains("Dec1") && !v_false.as_str().unwrap().contains("Pref1"));
        let narr = memoryLog(&json!({ "type": "narrative" }), &seeded()).unwrap();
        assert!(narr.as_str().unwrap().contains("\x1b[36mnarrative\x1b[0m")); // falls back to cyan
    }

    #[test]
    fn limit_and_singular_entry() {
        let r = memoryLog(&json!({ "limit": 1, "color": false }), &seeded()).unwrap();
        let s = r.as_str().unwrap();
        assert!(s.contains("Memory \u{b7} 1 entry")); // singular, not "1 entries"
        assert!(!s.contains("1 entries"));
    }
}
