//! interviewer-embed-turn.rs — twin of skills/agents/in/interviewer-embed-turn.mjs (db WRITE
//! lane). NAMES ARE CONTRACT: `interviewerEmbedTurn`. Embed a turn's user_text and store it in
//! interviewer_turns.user_text_embedding. Called inline after a turn row is inserted; failures
//! are non-fatal so the turn proceeds without an embedding.
//!
//! `context.db` missing → JS throws `err.contextMissing('interviewer-embed-turn', 'db')`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (same convention as
//! every sibling db-lane twin, e.g. `turn-widget-save.rs`).
//!
//! Dispatches to `crate::external_embed::embed`. JS wraps the `await embed(...)` + BLOB UPDATE in
//! try/catch and treats ANY failure as non-fatal (`{ stored: false, dims: 0, error: e?.message }`),
//! never throwing — reproduced here for both the embed error and the UPDATE error.
//!
//! HISTORY (fixed 2026-08-13): the success path used to be `unreachable!("embed twin never
//! returns Ok")`, written when `embed` was a stub that always Errd. Once fastembed landed, embed
//! started SUCCEEDING and this panicked. In the BE that is fatal, not just noisy: a Rust panic
//! crosses the napi boundary and aborts the entire Node process, so an ordinary chat turn could
//! kill the server. The store path (Float32Array -> Buffer -> `UPDATE interviewer_turns SET
//! user_text_embedding = ?` -> `{ stored: true, dims }`) is now implemented.
//!
//! ⚠️ 12 SIBLING `unreachable!()` CALLS REST ON THE SAME KIND OF STALE ASSUMPTION —
//! "aiCall always returns Err" — while ai-call.rs's own header now says the default export "is a
//! REAL implementation, not a stub". See deepsearch-run.rs, tl-contract-redraft.rs and
//! cockpit-plan-generate.rs. Each is a process-abort waiting for the first successful AI call.

use rusqlite::Connection;
use serde_json::{json, Value};

fn err_required(field: &str) -> String {
    format!("interviewer-embed-turn: {field} is required")
}

pub fn interviewerEmbedTurn(input: &Value, db: &Connection) -> Result<Value, String> {
    let turn_id = match input.get("turn_id") {
        Some(v) if truthy(v) => v.clone(),
        _ => return Err(err_required("turn_id")),
    };
    let user_text = match input.get("user_text") {
        Some(v) if truthy(v) => v.as_str().unwrap_or_default(),
        _ => return Err(err_required("user_text")),
    };

    match crate::external_embed::embed(&json!({ "text": user_text }), &Value::Null) {
        Ok(result) => {
            // Float32Array -> Buffer -> BLOB, little-endian f32, which is exactly what JS's
            // `Buffer.from(new Float32Array(v).buffer)` writes and what
            // interviewer-episodic-memory reads back with `f32::from_le_bytes`.
            let Some(vector) = result.get("vector").and_then(|v| v.as_array()) else {
                return Ok(json!({ "stored": false, "dims": 0, "error": "embed: no vector" }));
            };
            let mut bytes = Vec::with_capacity(vector.len() * 4);
            for n in vector {
                bytes.extend_from_slice(&(n.as_f64().unwrap_or(0.0) as f32).to_le_bytes());
            }
            let dims = vector.len();

            // JS wraps the whole embed + UPDATE in try/catch and treats ANY failure as
            // non-fatal, so a write error reports `stored:false` rather than failing the turn.
            match db.execute(
                "UPDATE interviewer_turns SET user_text_embedding = ? WHERE id = ?",
                rusqlite::params![bytes, turn_id.as_i64().unwrap_or_default()],
            ) {
                Ok(_) => Ok(json!({ "stored": true, "dims": dims })),
                Err(e) => Ok(json!({ "stored": false, "dims": 0, "error": e.to_string() })),
            }
        }
        Err(e) => Ok(json!({ "stored": false, "dims": 0, "error": e })),
    }
}

// JS `!user_text` / `!turn_id` falsy check (Number 0, "", null, undefined, false all reject).
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE interviewer_turns (id INTEGER PRIMARY KEY, user_text_embedding BLOB); \
             INSERT INTO interviewer_turns (id) VALUES (7);",
        )
        .unwrap();
        c
    }

    #[test]
    fn requires_turn_id() {
        let c = seed();
        let err = interviewerEmbedTurn(&json!({ "user_text": "hi" }), &c).unwrap_err();
        assert_eq!(err, "interviewer-embed-turn: turn_id is required");
    }

    #[test]
    fn falsy_turn_id_is_rejected() {
        let c = seed();
        let err = interviewerEmbedTurn(&json!({ "turn_id": 0, "user_text": "hi" }), &c).unwrap_err();
        assert_eq!(err, "interviewer-embed-turn: turn_id is required");
    }

    #[test]
    fn requires_user_text() {
        let c = seed();
        let err = interviewerEmbedTurn(&json!({ "turn_id": 7 }), &c).unwrap_err();
        assert_eq!(err, "interviewer-embed-turn: user_text is required");
    }

    #[test]
    fn empty_user_text_is_rejected() {
        let c = seed();
        let err = interviewerEmbedTurn(&json!({ "turn_id": 7, "user_text": "" }), &c).unwrap_err();
        assert_eq!(err, "interviewer-embed-turn: user_text is required");
    }

    #[test]
    fn embed_failure_is_non_fatal_and_reports_dims_zero() {
        let c = seed();
        let r = interviewerEmbedTurn(&json!({ "turn_id": 7, "user_text": "hello world" }), &c).unwrap();
        assert_eq!(r["stored"], json!(false));
        assert_eq!(r["dims"], json!(0));
        // embed is REAL now; under test it refuses rather than downloading weights. What this
        // test actually guards is unchanged and is the point: an embed failure must be NON-FATAL —
        // the turn still returns Ok with stored=false and dims=0, carrying the reason.
        assert!(r["error"].as_str().unwrap().contains("embed:"), "{}", r["error"]);
    }
}
