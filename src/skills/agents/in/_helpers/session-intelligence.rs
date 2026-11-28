//! session-intelligence.rs — twin of skills/agents/in/_helpers/session-intelligence.mjs (db-first helper).
//! NAMES ARE CONTRACT: `sessionIntelligenceDigest`. Markdown digest of a session's widgets/artefacts.
// QUALITY: 79/100 — best-effort read (fallback strings), per-item summarize, uid/title regex normalize
use rusqlite::Connection;
use serde_json::Value;
use std::sync::OnceLock;
use regex::Regex;

fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }

fn summarize(type_: &str, payload: &Value, cap: usize) -> String {
    if type_ == "artifact" {
        let it = payload.get("item").cloned().unwrap_or_else(|| if payload.is_object() { payload.clone() } else { serde_json::json!({}) });
        if let Some(article) = it.get("article").and_then(|v| v.as_str()) {
            return format!("a deterministic \"{article}\" article (its figures + prose are computed from the substrate)");
        }
        static WS: OnceLock<Regex> = OnceLock::new();
        let ws = WS.get_or_init(|| Regex::new(r"\s+").unwrap());
        let md = ws.replace_all(it.get("body_md").and_then(|v| v.as_str()).unwrap_or(""), " ").trim().to_string();
        return if !md.is_empty() { take_chars(&md, cap) } else { "a saved article".to_string() };
    }
    let s = serde_json::to_string(payload).unwrap_or_else(|_| "{}".into());
    if !s.is_empty() && s != "{}" && s != "null" { format!("data: {}", take_chars(&s, cap)) } else { String::new() }
}

pub fn sessionIntelligenceDigest(db: &Connection, session_id: &Value, opts: &Value) -> Value {
    let none = "(no widgets or artefacts generated in this session yet)";
    if session_id.is_null() { return Value::String(none.into()); }
    let max_items = opts.get("maxItems").and_then(|v| v.as_i64()).unwrap_or(24) as usize;
    let max_chars = opts.get("maxCharsEach").and_then(|v| v.as_i64()).unwrap_or(600) as usize;

    let rows = match crate::db_json::query_rows(db,
        "SELECT id, type, kind, payload_json FROM interviewer_session_widgets WHERE session_id = ? AND kind != 'session-context' ORDER BY id ASC",
        &[session_id.clone()]) {
        Ok(r) => r,
        Err(_) => return Value::String("(session intelligence unavailable — the widget store predates this DB)".into()),
    };
    let rows = rows.as_array().cloned().unwrap_or_default();
    if rows.is_empty() { return Value::String(none.into()); }

    static LEAD: OnceLock<Regex> = OnceLock::new();
    static DASH: OnceLock<Regex> = OnceLock::new();
    static NONWORD: OnceLock<Regex> = OnceLock::new();
    let lead = LEAD.get_or_init(|| Regex::new(r"^atlas-|^vibe-").unwrap());
    let dash = DASH.get_or_init(|| Regex::new(r"-").unwrap());
    let nonword = NONWORD.get_or_init(|| Regex::new(r"[^\w]+").unwrap());

    let start = rows.len().saturating_sub(max_items);
    let mut lines: Vec<String> = Vec::new();
    for r in &rows[start..] {
        let g = |k: &str| r.get(k).cloned().unwrap_or(Value::Null);
        let type_ = g("type"); let type_s = type_.as_str().unwrap_or("");
        let kind = g("kind"); let kind_s = kind.as_str().unwrap_or("");
        let id = g("id");
        let payload: Value = r.get("payload_json").and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str::<Value>(s).ok()).unwrap_or(Value::Null);
        let title = if type_s == "artifact" {
            payload.get("item").and_then(|i| i.get("title")).and_then(|v| v.as_str()).unwrap_or("artefact").to_string()
        } else {
            dash.replace_all(&lead.replace(kind_s, ""), " ").to_string()
        };
        let s = summarize(type_s, &payload, max_chars);
        let uid = format!("{}-w{}", nonword.replace_all(kind_s, "-"), if let Some(i) = id.as_i64() { i.to_string() } else { id.as_str().map(|x| x.to_string()).unwrap_or_else(|| id.to_string()) });
        let tail = if s.is_empty() { String::new() } else { format!(" — {s}") };
        lines.push(format!("- **{title}** ({type_s} · #{uid}){tail}"));
    }
    Value::String(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn digests() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE interviewer_session_widgets (id INTEGER, session_id INTEGER, type TEXT, kind TEXT, payload_json TEXT); \
          INSERT INTO interviewer_session_widgets VALUES (1,7,'artifact','atlas-overview','{\"item\":{\"title\":\"Overview\",\"body_md\":\"hi there\"}}'),(2,7,'widget','vibe-chart','{\"points\":[1,2]}');").unwrap();
        let d = sessionIntelligenceDigest(&c, &json!(7), &json!({}));
        let s = d.as_str().unwrap();
        assert!(s.contains("**Overview** (artifact · #atlas-overview-w1)"));
        assert!(s.contains("chart") && s.contains("data:"));
        assert_eq!(sessionIntelligenceDigest(&c, &Value::Null, &json!({})).as_str().unwrap(), "(no widgets or artefacts generated in this session yet)");
    }
}
