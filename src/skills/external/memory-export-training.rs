//! memory-export-training.rs — twin of skills/external/memory-export-training.mjs (db lane,
//! cross-db: memory.memories). NAMES ARE CONTRACT: `memoryExportTraining` (default). Emits
//! validated memories as a training JSONL string in one of 3 formats (sft/preference/tagged).
// QUALITY: 82/100 — the output "jsonl" field is a STRING built via per-row JSON.stringify, so the
// TEXT inside it is compared byte-for-byte: object key order must mirror the JS literal, and the
// REAL `confidence` column must render like a JS Number (no trailing ".0" on whole values) or a
// row like confidence=1 diverges as "1.0" vs "1" even though the outer object shape matches.
use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

// Matches JS Number-to-JSON rendering: a whole-valued float (e.g. the REAL `confidence` column
// holding 1) prints as "1", never "1.0". Only the fractional shortest-round-trip form differs
// (matches serde_json's ryu output for the typical decimal test values this column holds).
fn num_to_value(f: f64) -> Value {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.007_199_254_740_992e15 { json!(f as i64) } else { json!(f) }
}

// JSON.parse(m.tags_json || '[]') — tags_json is TEXT NOT NULL, so only the empty string is falsy.
fn parse_tags(m: &Value) -> Result<Value, String> {
    let s = m.get("tags_json").and_then(|v| v.as_str()).unwrap_or("");
    let raw = if s.is_empty() { "[]" } else { s };
    serde_json::from_str(raw).map_err(|e| e.to_string())
}

fn metadata(m: &Value, tags: Value) -> Value {
    json!({
        "memory_id": m.get("id").cloned().unwrap_or(Value::Null),
        "type": m.get("type").cloned().unwrap_or(Value::Null),
        "tags": tags,
    })
}

fn build_sft(m: &Value) -> Result<Option<Value>, String> {
    let system = m.get("situation").cloned().unwrap_or(Value::Null);
    let user = m.get("title").cloned().unwrap_or(Value::Null);
    let guidance = str_qq(m.get("guidance"));
    let rationale = m.get("rationale");
    let assistant = if truthy(rationale) { format!("{}\n\nWhy: {}", guidance, tmpl(rationale)) } else { guidance };
    let tags = parse_tags(m)?;
    Ok(Some(json!({
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
            { "role": "assistant", "content": assistant },
        ],
        "metadata": metadata(m, tags),
    })))
}

fn build_preference(m: &Value) -> Result<Option<Value>, String> {
    if !truthy(m.get("naive_response")) { return Ok(None); }
    let prompt = format!("{}\n\nTask: {}", tmpl(m.get("situation")), tmpl(m.get("title")));
    let guidance = str_qq(m.get("guidance"));
    let rationale = m.get("rationale");
    let chosen = if truthy(rationale) { format!("{}\n\nReasoning: {}", guidance, tmpl(rationale)) } else { guidance };
    let rejected = m.get("naive_response").cloned().unwrap_or(Value::Null);
    let tags = parse_tags(m)?;
    Ok(Some(json!({
        "prompt": prompt,
        "chosen": chosen,
        "rejected": rejected,
        "metadata": metadata(m, tags),
    })))
}

fn build_tagged(m: &Value) -> Result<Option<Value>, String> {
    let tags = parse_tags(m)?;
    let confidence = match m.get("confidence").and_then(|v| v.as_f64()) {
        Some(f) => num_to_value(f),
        None => m.get("confidence").cloned().unwrap_or(Value::Null),
    };
    Ok(Some(json!({
        "id": m.get("id").cloned().unwrap_or(Value::Null),
        "type": m.get("type").cloned().unwrap_or(Value::Null),
        "title": m.get("title").cloned().unwrap_or(Value::Null),
        "situation": m.get("situation").cloned().unwrap_or(Value::Null),
        "guidance": m.get("guidance").cloned().unwrap_or(Value::Null),
        "rationale": m.get("rationale").cloned().unwrap_or(Value::Null),
        "naive_response": m.get("naive_response").cloned().unwrap_or(Value::Null),
        "tags": tags,
        "source_card": m.get("source_card").cloned().unwrap_or(Value::Null),
        "confidence": confidence,
    })))
}

pub fn memoryExportTraining(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('memory-export-training','db') when no db — the db lane always injects one.
    let format = nz(input.get("format")).cloned().unwrap_or(json!("sft"));

    let mut sql = String::from("SELECT * FROM memory.memories WHERE superseded_by IS NULL AND validated = 1");
    let mut params: Vec<Value> = Vec::new();
    let type_filter = nz(input.get("type")).cloned().unwrap_or(Value::Null);
    if truthy(Some(&type_filter)) {
        sql.push_str(" AND type = ?");
        params.push(type_filter);
    }
    sql.push_str(" ORDER BY id ASC");

    let builder: fn(&Value) -> Result<Option<Value>, String> = match format.as_str() {
        Some("sft") => build_sft,
        Some("preference") => build_preference,
        Some("tagged") => build_tagged,
        _ => return Err(format!("memory-export-training: unknown format '{}'. Use: sft | preference | tagged", tmpl(Some(&format)))),
    };

    let rows_v = crate::db_json::query_rows(db, &sql, &params)?;
    let rows = rows_v.as_array().cloned().unwrap_or_default();

    let mut lines: Vec<String> = Vec::new();
    let mut emitted: i64 = 0;
    let mut skipped: i64 = 0;
    for r in &rows {
        match builder(r)? {
            None => skipped += 1,
            Some(obj) => {
                lines.push(serde_json::to_string(&obj).map_err(|e| e.to_string())?);
                emitted += 1;
            }
        }
    }

    Ok(json!({
        "format": format,
        "candidates": rows.len() as i64,
        "emitted": emitted,
        "skipped": skipped,
        "jsonl": lines.join("\n"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory;
            CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL, situation TEXT NOT NULL, guidance TEXT NOT NULL, naive_response TEXT, rationale TEXT, tags_json TEXT NOT NULL DEFAULT '[]', source_card TEXT, confidence REAL NOT NULL DEFAULT 0.5, validated INTEGER NOT NULL DEFAULT 0, superseded_by INTEGER, created_by TEXT NOT NULL);
            INSERT INTO memory.memories (id,type,title,situation,guidance,naive_response,rationale,tags_json,source_card,confidence,validated,superseded_by,created_by) VALUES
              (1,'lesson','Single-table beats N','Designing storage','Use one table + type',NULL,'Less bikeshedding','[]',NULL,1,1,NULL,'test'),
              (2,'correction','no naive','s','g',NULL,NULL,'[]',NULL,1,1,NULL,'test'),
              (3,'correction','with naive','s2','good answer','bad answer',NULL,'[]',NULL,0.9,1,NULL,'test'),
              (4,'lesson','X','s3','g3',NULL,NULL,'[\"a\",\"b\"]',NULL,1,1,NULL,'test'),
              (5,'lesson','not validated','s4','g4',NULL,NULL,'[]',NULL,1,0,NULL,'test'),
              (6,'lesson','old','s5','g5',NULL,NULL,'[]',NULL,1,1,4,'test');").unwrap();
        c
    }

    #[test]
    fn sft_builds_messages_and_appends_rationale() {
        let r = memoryExportTraining(&json!({ "format": "sft" }), &seeded()).unwrap();
        assert_eq!(r["candidates"], json!(4)); // id=5 excluded (not validated), id=6 excluded (superseded)
        let obj: Value = serde_json::from_str(r["jsonl"].as_str().unwrap().lines().next().unwrap()).unwrap();
        assert_eq!(obj["messages"][0], json!({ "role": "system", "content": "Designing storage" }));
        assert_eq!(obj["messages"][1], json!({ "role": "user", "content": "Single-table beats N" }));
        assert_eq!(obj["messages"][2]["content"], json!("Use one table + type\n\nWhy: Less bikeshedding"));
        assert_eq!(obj["metadata"], json!({ "memory_id": 1, "type": "lesson", "tags": [] }));
    }

    #[test]
    fn preference_skips_rows_without_naive_response() {
        let r = memoryExportTraining(&json!({ "format": "preference" }), &seeded()).unwrap();
        assert_eq!(r["candidates"], json!(4));
        assert_eq!(r["emitted"], json!(1));
        assert_eq!(r["skipped"], json!(3));
        let obj: Value = serde_json::from_str(r["jsonl"].as_str().unwrap()).unwrap();
        assert_eq!(obj["rejected"], json!("bad answer"));
        assert_eq!(obj["chosen"], json!("good answer"));
        assert_eq!(obj["prompt"], json!("s2\n\nTask: with naive"));
    }

    #[test]
    fn tagged_preserves_fields_and_renders_whole_confidence_without_decimal() {
        let r = memoryExportTraining(&json!({ "format": "tagged", "type": "lesson" }), &seeded()).unwrap();
        let obj: Value = serde_json::from_str(r["jsonl"].as_str().unwrap().lines().next().unwrap()).unwrap();
        assert_eq!(obj["title"], json!("Single-table beats N"));
        assert_eq!(obj["tags"], json!([]));
        // JSON.stringify(1) === "1" (not "1.0") — this is exactly the byte the harness compares.
        // confidence is the LAST key in the object, so it's followed by `}`, not `,`.
        assert!(r["jsonl"].as_str().unwrap().contains("\"confidence\":1}"));
        assert!(!r["jsonl"].as_str().unwrap().contains("1.0"));
    }

    #[test]
    fn tagged_keeps_fractional_confidence() {
        let r = memoryExportTraining(&json!({ "format": "preference" }), &seeded()).unwrap();
        assert!(r["jsonl"].as_str().unwrap().len() > 0); // sanity: preference path already covers row 3
        let r2 = memoryExportTraining(&json!({ "format": "tagged" }), &seeded()).unwrap();
        assert!(r2["jsonl"].as_str().unwrap().contains("\"confidence\":0.9"));
    }

    #[test]
    fn type_filter_narrows_candidates() {
        let r = memoryExportTraining(&json!({ "format": "sft", "type": "correction" }), &seeded()).unwrap();
        assert_eq!(r["candidates"], json!(2));
    }

    #[test]
    fn unknown_format_errors() {
        assert!(memoryExportTraining(&json!({ "format": "foo" }), &seeded()).is_err());
    }

    #[test]
    fn empty_result_set_yields_empty_jsonl() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory;
            CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL, situation TEXT NOT NULL, guidance TEXT NOT NULL, naive_response TEXT, rationale TEXT, tags_json TEXT NOT NULL DEFAULT '[]', source_card TEXT, confidence REAL NOT NULL DEFAULT 0.5, validated INTEGER NOT NULL DEFAULT 0, superseded_by INTEGER, created_by TEXT NOT NULL);").unwrap();
        let r = memoryExportTraining(&json!({}), &c).unwrap();
        assert_eq!(r["emitted"], json!(0));
        assert_eq!(r["jsonl"], json!(""));
    }
}
