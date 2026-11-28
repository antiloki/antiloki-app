//! keeper-memory-capture-correction.rs — twin of skills/agents/keeper/keeper-memory-capture-correction.mjs
//! (db WRITE lane, attached `memory` db). NAMES ARE CONTRACT: `keeperMemoryCaptureCorrection` (default).
//! Fires when a qa_fail is followed by a qa_pass on the same card: captures (mistake → fix →
//! guidance) as a type='correction' memory row so future sessions consult it.
//! Feature card: FEAT-MEMORY-CORRECTION-WORKFLOW.
// QUALITY: 84/100 — required-field guards, ??-vs-destructuring-default nulls kept distinct, undefined card_slug omitted
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` left-operand filter

pub fn keeperMemoryCaptureCorrection(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('keeper-memory-capture-correction','db') when no db — the db
    // lane always injects one, so that guard is unreachable here.
    for field in ["title", "situation", "naive_response", "guidance"] {
        if !truthy(input.get(field)) {
            return Err(format!("keeper-memory-capture-correction: {field} is required"));
        }
    }

    let title = input.get("title").cloned().unwrap_or(Value::Null);
    let situation = input.get("situation").cloned().unwrap_or(Value::Null);
    let guidance = input.get("guidance").cloned().unwrap_or(Value::Null);
    let naive_response = input.get("naive_response").cloned().unwrap_or(Value::Null);
    let rationale = nz(input.get("rationale")).cloned().unwrap_or(Value::Null); // rationale ?? null
    // `tags = []` destructuring default (absent) + `tags ?? []` (explicit null) collapse to the same []
    let tags = nz(input.get("tags")).cloned().unwrap_or(json!([]));
    let tags_json = serde_json::to_string(&tags).map_err(|e| e.to_string())?;
    let source_card = nz(input.get("card_slug")).cloned().unwrap_or(Value::Null); // card_slug ?? null
    // destructuring default fires only when the key is ABSENT; explicit null stays null (→ SQL NULL)
    let created_by = match input.get("created_by") { None => json!("keeper:auto"), Some(v) => v.clone() };

    let (_changes, rowid) = crate::db_json::run_write(
        db,
        "INSERT INTO memory.memories
      (type, title, situation, guidance, naive_response, rationale, tags_json,
       source_card, created_by, confidence, validated)
    VALUES ('correction', ?, ?, ?, ?, ?, ?, ?, ?, 0.7, 0)",
        &[title, situation, guidance, naive_response, rationale, json!(tags_json), source_card, created_by],
    )?;

    let mut out = Map::new();
    out.insert("id".into(), json!(rowid)); // Number(info.lastInsertRowid) — always integral
    out.insert("type".into(), json!("correction"));
    // JS returns the raw destructured card_slug: absent → undefined → key OMITTED by JSON.stringify;
    // explicit null stays a present null key.
    if let Some(cs) = input.get("card_slug") { out.insert("card_slug".into(), cs.clone()); }
    out.insert("captured_at".into(), json!(crate::clock::now_iso_nodb())); // frozen-clock lane: new Date().toISOString()
    out.insert("auto_consult".into(), json!("Loaded by interviewer-bundle-slice when injecting correction/lesson memories into context."));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory;
            CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL, situation TEXT NOT NULL, guidance TEXT NOT NULL, naive_response TEXT, rationale TEXT, tags_json TEXT NOT NULL DEFAULT '[]', source_card TEXT, confidence REAL NOT NULL DEFAULT 0.5, validated INTEGER NOT NULL DEFAULT 0, created_by TEXT NOT NULL);").unwrap();
        c
    }
    fn base() -> Value { json!({ "title": "t", "situation": "s", "naive_response": "n", "guidance": "g" }) }

    #[test]
    fn minimal_input_inserts_with_defaults_and_omits_card_slug() {
        let c = db();
        let r = keeperMemoryCaptureCorrection(&base(), &c).unwrap();
        // key order is part of the contract: card_slug omitted when absent
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"id":1,"type":"correction","captured_at":"2026-01-01T00:00:00.000Z","auto_consult":"Loaded by interviewer-bundle-slice when injecting correction/lesson memories into context."}"#
        );
        let row = crate::db_json::query_rows(&c, "SELECT * FROM memory.memories WHERE id = 1", &[]).unwrap()[0].clone();
        assert_eq!(row["type"], json!("correction"));
        assert_eq!(row["created_by"], json!("keeper:auto"));
        assert_eq!(row["tags_json"], json!("[]"));
        assert_eq!(row["rationale"], Value::Null);
        assert_eq!(row["source_card"], Value::Null);
        assert_eq!(row["confidence"], json!(0.7));
        assert_eq!(row["validated"], json!(0));
    }

    #[test]
    fn full_input_threads_values_and_returns_next_rowid() {
        let c = db();
        c.execute_batch("INSERT INTO memory.memories (id,type,title,situation,guidance,naive_response,rationale,tags_json,source_card,confidence,validated,created_by) VALUES (1,'lesson','T1','S1','G1',NULL,NULL,'[]',NULL,0.5,0,'test');").unwrap();
        let r = keeperMemoryCaptureCorrection(&json!({
            "card_slug": "CARD-X", "title": "t", "situation": "s", "naive_response": "n",
            "guidance": "g", "rationale": "why", "tags": ["a", "b"], "created_by": "human:dani",
            "agent_slug": "dev"
        }), &c).unwrap();
        assert_eq!(r["id"], json!(2));
        assert_eq!(r["card_slug"], json!("CARD-X"));
        let row = crate::db_json::query_rows(&c, "SELECT * FROM memory.memories WHERE id = 2", &[]).unwrap()[0].clone();
        assert_eq!(row["source_card"], json!("CARD-X"));
        assert_eq!(row["rationale"], json!("why"));
        assert_eq!(row["tags_json"], json!("[\"a\",\"b\"]"));
        assert_eq!(row["created_by"], json!("human:dani"));
    }

    #[test]
    fn explicit_nulls_keep_card_slug_key_and_default_tags() {
        let c = db();
        let mut input = base();
        input["card_slug"] = Value::Null;
        input["rationale"] = Value::Null;
        input["tags"] = Value::Null;
        let r = keeperMemoryCaptureCorrection(&input, &c).unwrap();
        assert!(r.as_object().unwrap().contains_key("card_slug"));
        assert_eq!(r["card_slug"], Value::Null);
        let row = crate::db_json::query_rows(&c, "SELECT tags_json, source_card FROM memory.memories WHERE id = 1", &[]).unwrap()[0].clone();
        assert_eq!(row["tags_json"], json!("[]"));
        assert_eq!(row["source_card"], Value::Null);
    }

    #[test]
    fn missing_required_fields_err() {
        let c = db();
        for field in ["title", "situation", "naive_response", "guidance"] {
            let mut input = base();
            input.as_object_mut().unwrap().remove(field);
            let e = keeperMemoryCaptureCorrection(&input, &c).unwrap_err();
            assert!(e.contains(&format!("{field} is required")), "{e}");
        }
        // falsy empty string trips the guard too (JS `if (!title)`)
        let mut input = base();
        input["title"] = json!("");
        assert!(keeperMemoryCaptureCorrection(&input, &c).unwrap_err().contains("title is required"));
    }
}
