//! data-story-to-card.rs — twin of skills/codebase/data-story-to-card.mjs (db WRITE lane). NAMES
//! ARE CONTRACT: `dataStoryToCard` (named default). When a data story (call/import journey) is
//! BROKEN, draft a backlog card to fix it — reuses data-story-get's live verification + card-add.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }

// = JS Array.prototype.join(sep): null/undefined elements render as '' (not "null").
fn js_array_join(arr: &[Value], sep: &str) -> String {
    arr.iter().map(|v| match v { Value::Null => String::new(), other => js_string(other) }).collect::<Vec<_>>().join(sep)
}

// six hex uppercase chars (mirrors randomUUID().slice(0,6).toUpperCase()); NOT byte-identical to
// node:crypto's RNG (never exercised by conform fixtures — those always pass an explicit slug).
fn random_hex6() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let mut x = (nanos as u64) ^ 0x9E3779B97F4A7C15u64;
    x ^= x << 13; x ^= x >> 7; x ^= x << 17;
    format!("{:06X}", x & 0xFFFFFF)
}

// = JS label: h.label ?? h.meta?.name ?? h.node_id
fn label(h: &Value) -> Value {
    if let Some(l) = h.get("label") { if !l.is_null() { return l.clone(); } }
    if let Some(m) = h.get("meta") { if !m.is_null() {
        if let Some(n) = m.get("name") { if !n.is_null() { return n.clone(); } }
    }}
    h.get("node_id").cloned().unwrap_or(Value::Null)
}

pub fn dataStoryToCard(input: &Value, db: &Connection) -> Result<Value, String> {
    let id = input.get("id");
    if id.map(|v| v.is_null()).unwrap_or(true) { return Err("data-story-to-card: id is required".into()); }
    let id = id.unwrap().clone();
    let origin = input.get("origin").cloned().unwrap_or(Value::Null);

    let get_input = json!({ "id": id, "origin": origin });
    let story = crate::codebase_data_story_get::dataStoryGet(&get_input, db)?;

    let status = story.get("status").and_then(|v| v.as_str()).unwrap_or("");
    let name = tmpl(story.get("name"));
    if status != "broken" {
        return Ok(json!({
            "ok": false,
            "reason": format!("story \"{}\" is {}, not broken \u{2014} nothing to fix", name, status),
            "story_id": story.get("id").cloned().unwrap_or(Value::Null),
        }));
    }

    let hops = story.get("hops").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let mut labels: Vec<Value> = Vec::new();
    let mut broken_hops: Vec<Value> = Vec::new();
    let mut missing_hops: Vec<Value> = Vec::new();
    let mut files: Vec<Value> = Vec::new();
    let mut seen_files: std::collections::HashSet<String> = std::collections::HashSet::new();

    for h in &hops {
        let lbl = label(h);
        labels.push(lbl.clone());
        if h.get("edge_ok") == Some(&json!(false)) { broken_hops.push(lbl); }
        let exists = h.get("exists").and_then(|v| v.as_bool()).unwrap_or(false);
        if !exists { missing_hops.push(h.get("node_id").cloned().unwrap_or(Value::Null)); }
        if let Some(m) = h.get("meta") { if !m.is_null() {
            if let Some(p) = m.get("path") {
                if truthy(Some(p)) {
                    let key = js_string(p);
                    if seen_files.insert(key) { files.push(p.clone()); }
                }
            }
        }}
    }

    let journey = js_array_join(&labels, " \u{2192} ");
    let broken_join = js_array_join(&broken_hops, ", ");
    let missing_join = js_array_join(&missing_hops, ", ");
    let direction = story.get("direction").and_then(|v| v.as_str()).unwrap_or("");

    let slug = match input.get("slug") {
        Some(s) if !s.is_null() => str_qq(Some(s)),
        _ => format!("FIX-STORY-{}", random_hex6()),
    };

    let motivation = format!(
        "The data-flow journey \"{}\" is broken: it no longer connects{}{}. Restore the expected call/import flow so the journey reconnects end-to-end \u{2014} or, if the architecture changed on purpose, update the story to match the new flow.",
        name,
        if !broken_hops.is_empty() { format!(" at {}", broken_join) } else { String::new() },
        if !missing_hops.is_empty() { format!(" (missing: {})", missing_join) } else { String::new() },
    );

    let mut context_lines: Vec<Value> = Vec::new();
    context_lines.push(json!(format!("Journey ({}): {}", if direction == "backward" { "from-db" } else { "from-user" }, journey)));
    if !broken_hops.is_empty() { context_lines.push(json!(format!("Broken transition(s): {}", broken_join))); }
    if !missing_hops.is_empty() { context_lines.push(json!(format!("Missing node(s): {}", missing_join))); }
    context_lines.push(json!("Detected by the atlas data-story verification, checked live against the call/import graph."));

    let acceptance_criteria = vec![
        json!(format!("The journey reconnects end-to-end: {} \u{2014} every hop has a call/import edge in the atlas.", journey)),
        json!(format!("Re-verifying the data story \"{}\" reports status \"ok\" (0 broken hops).", name)),
    ];

    let diagnosis = format!(
        "{} broken transition(s) + {} missing node(s) along the journey.",
        tmpl(story.get("broken")), tmpl(story.get("missing")),
    );

    let reproduction = vec![
        json!(format!("Open flow mode on the Atlas graph and play the story \"{}\".", name)),
        json!(format!("Observe the red hop(s){} \u{2014} the flow stops there.", if !broken_hops.is_empty() { format!(" at {}", broken_join) } else { String::new() })),
    ];

    let mut body = Map::new();
    body.insert("kind".into(), json!("fix"));
    body.insert("size".into(), json!("S"));
    body.insert("motivation".into(), json!(motivation));
    body.insert("context".into(), Value::Array(context_lines));
    body.insert("acceptance_criteria".into(), Value::Array(acceptance_criteria));
    body.insert("diagnosis".into(), json!(diagnosis));
    body.insert("reproduction".into(), Value::Array(reproduction));
    if !files.is_empty() { body.insert("files_touched".into(), Value::Array(files)); }

    let title = format!("Fix broken data story: {}", name);
    let card_input = json!({ "slug": slug, "title": title, "status": "backlog", "body": Value::Object(body) });
    let card = crate::entities_card_add::cardAdd(&card_input, db)?;

    Ok(json!({
        "card_slug": card.get("slug").cloned().unwrap_or(Value::Null),
        "title": card.get("title").cloned().unwrap_or(Value::Null),
        "story_id": story.get("id").cloned().unwrap_or(Value::Null),
        "broken": story.get("broken").cloned().unwrap_or(Value::Null),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT, origin TEXT, name TEXT, path TEXT, type TEXT); \
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT); \
             CREATE TABLE code_data_stories (id INTEGER, origin TEXT, name TEXT, description TEXT, direction TEXT, hops_json TEXT, linked_test TEXT, created_at TEXT, updated_at TEXT); \
             CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT UNIQUE, title TEXT, status TEXT, body_json TEXT, parent_slug TEXT); \
             INSERT INTO code_nodes VALUES ('a','atlas','a','a.ts','function'),('b','atlas','b','b.ts','function'); \
             INSERT INTO code_edges (origin,from_node,to_node,type) VALUES ('atlas','a','b','calls'); \
             INSERT INTO code_data_stories VALUES (1,'atlas','Login Flow','d','forward','[{\"node_id\":\"a\"},{\"node_id\":\"b\"},{\"node_id\":\"c\"}]',NULL,NULL,NULL); \
             INSERT INTO code_data_stories VALUES (2,'atlas','Simple Flow','d2','forward','[{\"node_id\":\"a\"},{\"node_id\":\"b\"}]',NULL,NULL,NULL);",
        ).unwrap();
        c
    }

    #[test]
    fn broken_story_makes_a_fix_card() {
        let c = setup();
        let r = dataStoryToCard(&json!({ "id": 1, "origin": "atlas", "slug": "FIX-STORY-TEST01" }), &c).unwrap();
        assert_eq!(r["card_slug"], json!("FIX-STORY-TEST01"));
        assert_eq!(r["title"], json!("Fix broken data story: Login Flow"));
        assert_eq!(r["broken"], json!(1));

        let row: String = c.query_row("SELECT body_json FROM cards WHERE slug = ?", ["FIX-STORY-TEST01"], |r| r.get(0)).unwrap();
        let body: Value = serde_json::from_str(&row).unwrap();
        assert_eq!(body["kind"], json!("fix"));
        assert!(body["motivation"].as_str().unwrap().contains("Login Flow"));
        assert!(body["motivation"].as_str().unwrap().contains("(missing: c)"));
        assert_eq!(body["files_touched"], json!(["a.ts", "b.ts"]));
    }

    #[test]
    fn ok_story_refuses() {
        let c = setup();
        let r = dataStoryToCard(&json!({ "id": 2, "origin": "atlas" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["story_id"], json!(2));
        let count: i64 = c.query_row("SELECT COUNT(*) FROM cards", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn requires_id() {
        let c = setup();
        assert!(dataStoryToCard(&json!({}), &c).is_err());
    }
}
