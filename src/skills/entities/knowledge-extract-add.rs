//! knowledge-extract-add.rs — twin of skills/entities/knowledge-extract-add.mjs (db WRITE lane,
//! cross-db via ATTACH). NAMES ARE CONTRACT: `knowledgeExtractAdd` (default). Register a curated
//! insight from a vault document; the raw doc stays in the vault — this row is the AI-consultable
//! distillation. Validates extract_type against the allowed enum, checks the parent document
//! exists, INSERTs into knowledge.extracts, and returns the inserted row (SELECT * read-back).
use rusqlite::Connection;
use serde_json::{json, Value};

const ALLOWED_EXTRACT_TYPES: [&str; 7] = ["quote", "figure", "insight", "code", "diagram", "summary", "note"];

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `${v}` interpolation (for the err.enumVal / err.notFound message payloads)
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

pub fn knowledgeExtractAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS destructures { db, actor = 'unknown' } from context; the harness context is { db } only,
    // so `actor` is always 'unknown' here (created_by's fallback below).
    let document_id = input.get("document_id");
    // JS Number.isInteger: numbers only (strings never pass), integral finite value
    let is_int = matches!(document_id, Some(Value::Number(n)) if n.as_f64().map_or(false, |f| f.is_finite() && f.fract() == 0.0));
    if !is_int {
        return Err("knowledge-extract-add: document_id is required".into());
    }
    let document_id = document_id.cloned().unwrap_or(Value::Null);

    let extract_type = input.get("extract_type");
    if !truthy(extract_type) {
        return Err("knowledge-extract-add: extract_type is required".into());
    }
    let in_enum = extract_type.and_then(|v| v.as_str()).map_or(false, |s| ALLOWED_EXTRACT_TYPES.contains(&s));
    if !in_enum {
        return Err(format!(
            "knowledge-extract-add: extract_type must be one of: {} (got '{}')",
            ALLOWED_EXTRACT_TYPES.join(", "),
            tmpl(extract_type)
        ));
    }
    let extract_type = extract_type.cloned().unwrap_or(Value::Null);

    // JS: `if (!text || typeof text !== 'string') throw` — exactly a non-empty string passes
    let text = match input.get("text") {
        Some(Value::String(s)) if !s.is_empty() => json!(s),
        _ => return Err("knowledge-extract-add: text is required".into()),
    };

    let doc = crate::db_json::query_rows(db, "SELECT id FROM knowledge.documents WHERE id = ?", &[document_id.clone()])?;
    if doc.as_array().map_or(true, |a| a.is_empty()) {
        return Err(format!("knowledge-extract-add: document '{}' not found", tmpl(Some(&document_id))));
    }

    // destructuring defaults fire only on ABSENT keys — explicit null stays null
    let extract_context = input.get("context").cloned().unwrap_or(Value::Null);
    let page_or_section = input.get("page_or_section").cloned().unwrap_or(Value::Null);
    let created_by = match input.get("created_by") {
        None => json!("unknown"), // = actor default
        Some(v) => v.clone(),
    };

    let (_changes, id) = crate::db_json::run_write(
        db,
        "\n    INSERT INTO knowledge.extracts\n      (document_id, extract_type, text, context, page_or_section, created_by)\n    VALUES (?, ?, ?, ?, ?, ?)\n  ",
        &[document_id, extract_type, text, extract_context, page_or_section, created_by],
    )?;

    let rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.extracts WHERE id = ?", &[json!(id)])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS knowledge;
            CREATE TABLE knowledge.documents (id INTEGER PRIMARY KEY, vault_path TEXT, doc_type TEXT, sha256 TEXT, bytes INTEGER, summary TEXT, ingested_at TEXT, ingested_by TEXT, external_source TEXT, superseded_by INTEGER, superseded_at TEXT);
            CREATE TABLE knowledge.extracts (id INTEGER PRIMARY KEY, document_id INTEGER, extract_type TEXT, text TEXT, context TEXT, page_or_section TEXT, created_at TEXT, created_by TEXT);
            INSERT INTO knowledge.documents (id, vault_path, doc_type, sha256, bytes, summary, ingested_at, ingested_by) VALUES
              (1,'books/alpha.md','md','aaa',100,'alpha book','2025-12-01 10:00:00','tester'),
              (2,'papers/beta.pdf','pdf','bbb',200,'beta paper','2025-12-02 10:00:00','tester');",
        )
        .unwrap();
        c
    }

    #[test]
    fn full_input_inserts_and_returns_the_row() {
        let c = seeded();
        let r = knowledgeExtractAdd(
            &json!({ "document_id": 1, "extract_type": "quote", "text": "Knowledge is provenance.", "context": "intro paragraph", "page_or_section": "page 7", "created_by": "curator" }),
            &c,
        )
        .unwrap();
        assert_eq!(
            r,
            json!({ "id": 1, "document_id": 1, "extract_type": "quote", "text": "Knowledge is provenance.", "context": "intro paragraph", "page_or_section": "page 7", "created_at": null, "created_by": "curator" })
        );
    }

    #[test]
    fn defaults_explicit_nulls_and_rowid_advance() {
        let c = seeded();
        let r = knowledgeExtractAdd(&json!({ "document_id": 1, "extract_type": "insight", "text": "Distill before you embed." }), &c).unwrap();
        assert_eq!(r["context"], Value::Null);
        assert_eq!(r["page_or_section"], Value::Null);
        assert_eq!(r["created_by"], json!("unknown")); // actor default (harness context has no actor)
        // explicit null created_by does NOT trigger the destructure default
        let r2 = knowledgeExtractAdd(&json!({ "document_id": 2, "extract_type": "note", "text": "explicit nulls stay null", "context": null, "page_or_section": null, "created_by": null }), &c).unwrap();
        assert_eq!(r2["created_by"], Value::Null);
        assert_eq!(r2["id"], json!(2)); // lastInsertRowid advances
    }

    #[test]
    fn guards_reject_and_leave_no_row() {
        let c = seeded();
        assert!(knowledgeExtractAdd(&json!({ "extract_type": "quote", "text": "t" }), &c).unwrap_err().contains("document_id is required"));
        assert!(knowledgeExtractAdd(&json!({ "document_id": 1.5, "extract_type": "quote", "text": "t" }), &c).unwrap_err().contains("document_id is required"));
        assert!(knowledgeExtractAdd(&json!({ "document_id": "1", "extract_type": "quote", "text": "t" }), &c).unwrap_err().contains("document_id is required"));
        assert!(knowledgeExtractAdd(&json!({ "document_id": 1, "text": "t" }), &c).unwrap_err().contains("extract_type is required"));
        assert!(knowledgeExtractAdd(&json!({ "document_id": 1, "extract_type": "sonnet", "text": "t" }), &c).unwrap_err().contains("must be one of"));
        assert!(knowledgeExtractAdd(&json!({ "document_id": 1, "extract_type": "quote" }), &c).unwrap_err().contains("text is required"));
        assert!(knowledgeExtractAdd(&json!({ "document_id": 1, "extract_type": "quote", "text": "" }), &c).unwrap_err().contains("text is required"));
        assert!(knowledgeExtractAdd(&json!({ "document_id": 99, "extract_type": "quote", "text": "t" }), &c).unwrap_err().contains("document '99' not found"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.extracts", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }
}
