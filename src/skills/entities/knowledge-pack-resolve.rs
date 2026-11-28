//! knowledge-pack-resolve.rs — twin of skills/entities/knowledge-pack-resolve.mjs (db lane,
//! cross-db via ATTACH: memory.memories, knowledge.documents). NAMES ARE CONTRACT:
//! `knowledgePackResolve` (default). Resolves a tag list (explicit array, or via card_tags for a
//! card_slug), then builds a token-budgeted context pack: tag-matched memories (active + on-type)
//! followed by best-effort knowledge docs, sharing ONE running total_chars budget across both.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)
// Math.max(200, N) propagates NaN in JS; Rust's f64::max ignores NaN, so hand-roll it.
fn js_max(a: f64, b: f64) -> f64 { if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) } }

const CHARS_PER_TOKEN: f64 = 4.0;

pub fn knowledgePackResolve(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('knowledge-pack-resolve','db') when no db — the db lane always injects one.
    let card_slug = input.get("card_slug").cloned().unwrap_or(Value::Null);

    // Array.isArray(explicitTags) ? explicitTags : null — any array (even []) short-circuits the
    // card_tags lookup below; anything else (absent/null/non-array) falls through to it.
    let explicit_tags = input.get("tags").and_then(|v| v.as_array()).cloned();
    let tags: Vec<Value> = match explicit_tags {
        Some(t) => t,
        None => {
            if truthy(input.get("card_slug")) {
                let rows = crate::db_json::query_rows(db, "SELECT tag_slug FROM card_tags WHERE card_slug = ?", &[card_slug.clone()])?;
                rows.as_array().unwrap().iter().map(|r| r.get("tag_slug").cloned().unwrap_or(Value::Null)).collect()
            } else {
                Vec::new()
            }
        }
    };

    if tags.is_empty() {
        return Ok(json!({ "card_slug": card_slug, "tags": [], "pack": { "memories": [], "knowledge": [], "total_chars": 0, "total_tokens_est": 0 } }));
    }

    let max_tokens = input.get("max_tokens").cloned().unwrap_or(json!(2000));
    let budget_chars = js_max(200.0, js_number(Some(&max_tokens)) * CHARS_PER_TOKEN);

    let all_mems = crate::db_json::query_rows(
        db,
        "SELECT id, type, title, guidance, tags_json FROM memory.memories WHERE COALESCE(superseded_by,'') = '' AND type IN ('lesson','correction','decision','preference','narrative') LIMIT 500",
        &[],
    )?;
    let mem_matched: Vec<Value> = all_mems
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|m| {
            let tj = m.get("tags_json").and_then(|v| v.as_str());
            let mt: Value = tj.and_then(|s| serde_json::from_str(s).ok()).unwrap_or_else(|| json!([]));
            match mt.as_array() {
                Some(arr) => arr.iter().any(|t| tags.iter().any(|tag| tag == t)),
                None => false,
            }
        })
        .collect();

    // Knowledge docs: best-effort (JS wraps this in try/catch — a schema mismatch yields []).
    let know_docs = crate::db_json::query_rows(db, "SELECT id, title, content_md FROM knowledge.documents LIMIT 200", &[])
        .ok()
        .and_then(|r| r.as_array().cloned())
        .unwrap_or_default();

    let mut memories_out: Vec<Value> = Vec::new();
    let mut knowledge_out: Vec<Value> = Vec::new();
    let mut total_chars: i64 = 0;
    for m in &mem_matched {
        let snippet: String = str_qq(m.get("guidance")).chars().take(280).collect();
        let snip_len = snippet.chars().count() as i64;
        if (total_chars + snip_len) as f64 > budget_chars { break; }
        memories_out.push(json!({
            "id": m.get("id").cloned().unwrap_or(Value::Null),
            "type": m.get("type").cloned().unwrap_or(Value::Null),
            "title": m.get("title").cloned().unwrap_or(Value::Null),
            "snippet": snippet,
        }));
        total_chars += snip_len;
    }
    for d in &know_docs {
        if (total_chars as f64) > budget_chars { break; }
        let snippet: String = str_qq(d.get("content_md")).chars().take(500).collect();
        knowledge_out.push(json!({
            "id": d.get("id").cloned().unwrap_or(Value::Null),
            "title": d.get("title").cloned().unwrap_or(Value::Null),
            "snippet": snippet,
        }));
        total_chars += snippet.chars().count() as i64;
    }
    let total_tokens_est = (total_chars as f64 / CHARS_PER_TOKEN).round() as i64;

    let mut pack = Map::new();
    pack.insert("memories".into(), json!(memories_out));
    pack.insert("knowledge".into(), json!(knowledge_out));
    pack.insert("total_chars".into(), json!(total_chars));
    pack.insert("total_tokens_est".into(), json!(total_tokens_est));

    let mut out = Map::new();
    out.insert("card_slug".into(), card_slug);
    out.insert("tags".into(), json!(tags));
    out.insert("pack".into(), Value::Object(pack));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
             ATTACH ':memory:' AS knowledge;
             CREATE TABLE card_tags (card_slug TEXT, tag_slug TEXT);
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, type TEXT, title TEXT, guidance TEXT, tags_json TEXT, superseded_by INTEGER);
             CREATE TABLE knowledge.documents (id INTEGER PRIMARY KEY, title TEXT, content_md TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn no_tags_returns_empty_pack() {
        let c = seeded();
        let r = knowledgePackResolve(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "card_slug": null, "tags": [], "pack": { "memories": [], "knowledge": [], "total_chars": 0, "total_tokens_est": 0 } }));
    }

    #[test]
    fn resolves_tags_via_card_tags_and_filters_memories() {
        let c = seeded();
        c.execute_batch(
            "INSERT INTO card_tags VALUES ('CARD-1','ai'), ('CARD-1','rust');
             INSERT INTO memory.memories (id,type,title,guidance,tags_json,superseded_by) VALUES
               (1,'lesson','L1','guidance one','[\"ai\"]',NULL),
               (2,'preference','L2','guidance two','[\"other\"]',NULL),
               (3,'decision','L3-superseded','should be excluded','[\"ai\"]',1),
               (4,'reference','L4-wrong-type','wrong type excluded','[\"ai\"]',NULL),
               (5,'correction','L5-malformed-json','malformed json excluded','{not json',NULL),
               (6,'narrative','L6-non-array-json','non array json excluded','{\"x\":\"ai\"}',NULL);
             INSERT INTO knowledge.documents (id,title,content_md) VALUES (1,'Doc A','body a');",
        )
        .unwrap();
        let r = knowledgePackResolve(&json!({ "card_slug": "CARD-1" }), &c).unwrap();
        assert_eq!(r["tags"], json!(["ai", "rust"]));
        assert_eq!(r["pack"]["memories"], json!([{ "id": 1, "type": "lesson", "title": "L1", "snippet": "guidance one" }]));
        assert_eq!(r["pack"]["knowledge"], json!([{ "id": 1, "title": "Doc A", "snippet": "body a" }]));
        assert_eq!(r["pack"]["total_chars"], json!(18));
        assert_eq!(r["pack"]["total_tokens_est"], json!(5)); // Math.round(4.5) = 5
    }

    #[test]
    fn explicit_tags_bypass_card_tags_lookup() {
        let c = seeded();
        c.execute_batch("INSERT INTO memory.memories (id,type,title,guidance,tags_json,superseded_by) VALUES (1,'lesson','L1','g','[\"py\"]',NULL);").unwrap();
        let r = knowledgePackResolve(&json!({ "card_slug": "IGNORED", "tags": ["py"] }), &c).unwrap();
        assert_eq!(r["card_slug"], json!("IGNORED"));
        assert_eq!(r["tags"], json!(["py"]));
        assert_eq!(r["pack"]["memories"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn explicit_empty_tags_short_circuits_even_with_card_tags_rows() {
        let c = seeded();
        c.execute_batch("INSERT INTO card_tags VALUES ('CARD-1','ai');").unwrap();
        let r = knowledgePackResolve(&json!({ "card_slug": "CARD-1", "tags": [] }), &c).unwrap();
        assert_eq!(r, json!({ "card_slug": "CARD-1", "tags": [], "pack": { "memories": [], "knowledge": [], "total_chars": 0, "total_tokens_est": 0 } }));
    }

    #[test]
    fn truncation_shares_one_running_budget_across_both_loops() {
        let c = seeded();
        let x = "x".repeat(100);
        let y = "y".repeat(100);
        let z = "z".repeat(100);
        let w = "w".repeat(50);
        let v = "v".repeat(50);
        c.execute(
            "INSERT INTO memory.memories (id,type,title,guidance,tags_json,superseded_by) VALUES (1,'lesson','L1',?1,'[\"ai\"]',NULL), (2,'lesson','L2',?2,'[\"ai\"]',NULL), (3,'lesson','L3-dropped',?3,'[\"ai\"]',NULL)",
            rusqlite::params![x, y, z],
        ).unwrap();
        c.execute(
            "INSERT INTO knowledge.documents (id,title,content_md) VALUES (1,'DocKept',?1), (2,'DocDropped',?2)",
            rusqlite::params![w, v],
        ).unwrap();
        let r = knowledgePackResolve(&json!({ "tags": ["ai"], "max_tokens": 10 }), &c).unwrap(); // budget = max(200, 40) = 200
        let mem_ids: Vec<i64> = r["pack"]["memories"].as_array().unwrap().iter().map(|m| m["id"].as_i64().unwrap()).collect();
        let know_ids: Vec<i64> = r["pack"]["knowledge"].as_array().unwrap().iter().map(|d| d["id"].as_i64().unwrap()).collect();
        assert_eq!(mem_ids, vec![1, 2]); // 100 then 200 <= 200; third would push to 300 > 200
        assert_eq!(know_ids, vec![1]); // 200 <= 200 keeps first doc; 250 > 200 drops second
        assert_eq!(r["pack"]["total_chars"], json!(250));
        assert_eq!(r["pack"]["total_tokens_est"], json!(63)); // Math.round(62.5) = 63
    }

    #[test]
    fn max_tokens_as_numeric_string_coerces() {
        let c = seeded();
        c.execute_batch("INSERT INTO memory.memories (id,type,title,guidance,tags_json,superseded_by) VALUES (1,'lesson','L1','short','[\"ai\"]',NULL);").unwrap();
        let r = knowledgePackResolve(&json!({ "tags": ["ai"], "max_tokens": "2000" }), &c).unwrap();
        assert_eq!(r["pack"]["total_chars"], json!(5));
        assert_eq!(r["pack"]["total_tokens_est"], json!(1)); // Math.round(1.25) = 1
    }
}
