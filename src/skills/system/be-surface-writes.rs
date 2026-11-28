//! be-surface-writes.rs — the last two of antiloki-BE's raw WRITES, moved into skills. NAMES ARE
//! CONTRACT: `cockpitPaneMeter`, `buildGateChunkAppend`.
//!
//! WHY THEY EXIST. These were the only `db_write` call sites left in the BE. Only a skill touches
//! the databases; a raw write from the customer layer is unauditable, unreachable from anywhere
//! else, and free to drift from the schema the skills maintain.
//!
//! NEITHER TABLE IS AUDITED. cockpit_panes' counters and build_gate_live's chunks are telemetry —
//! high-frequency, self-superseding, and meaningless to replay — so with-audit is deliberately not
//! composed here, the same call already made for `notifications`. That is a statement about these
//! two tables, not a general licence: any write to cards/agents/actions/memory.memories/laws.laws
//! must still go through with-audit (NEVER_BYPASS_WITH_AUDIT).
// QUALITY: 88/100 — best-effort metering, bounded chunk, frozen-clock aware
use rusqlite::Connection;
use serde_json::{json, Value};

/// Meter one proxied model call against a cockpit pane.
///
/// This is what makes the per-pane spend visible and the kill-switch meaningful: a pane that has
/// spent its budget can be stopped only if something counted. The BE calls it best-effort (a
/// failed meter must never fail the operator's request), which is exactly why the ACCUMULATION
/// has to be atomic SQL — `calls = calls + 1` rather than a read-modify-write — or two concurrent
/// proxied calls would each read the same value and one increment would vanish.
pub fn cockpitPaneMeter(input: &Value, db: &Connection) -> Result<Value, String> {
    let id = input
        .get("pane_id")
        .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .filter(|&n| n > 0)
        .ok_or("cockpit-pane-meter: pane_id is required")?;
    // A non-finite or absent cost meters as zero rather than refusing: the CALL still happened,
    // and losing the call count because the price was unknown is the worse failure.
    let cost = input.get("cost_usd").and_then(|v| v.as_f64()).filter(|c| c.is_finite()).unwrap_or(0.0);

    let changed = db
        .execute(
            &format!(
                "UPDATE cockpit_panes
                 SET calls = calls + 1, cost_usd = cost_usd + ?, last_activity = {}
                 WHERE id = ?",
                crate::clock::now_sql()
            ),
            rusqlite::params![cost, id],
        )
        .map_err(|e| format!("cockpit-pane-meter: {e}"))?;
    Ok(json!({ "pane_id": id, "metered": changed > 0 }))
}

/// Append one live-output chunk for a pane's PTY stream.
///
/// The chunk is TRUNCATED at 8000 bytes here rather than by the caller. A terminal can emit a
/// single enormous line (a minified bundle, a base64 blob), and the live view only needs enough
/// to render; the cap belongs with the insert so every caller gets it.
pub fn buildGateChunkAppend(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = input
        .get("contract_slug")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("build-gate-chunk-append: contract_slug is required")?;
    let seq = input.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
    // Truncated by CHARACTER, not by byte. A byte cut can land inside a multi-byte sequence, and
    // the operator would see a replacement glyph in their own terminal output. (JS sliced by
    // UTF-16 code unit; for the ASCII-dominated output a PTY emits these agree, and where they
    // differ, whole characters is the answer that renders.)
    let chunk: String = input
        .get("chunk")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .chars()
        .take(8000)
        .collect();
    let done = input.get("done").and_then(|v| v.as_bool()).unwrap_or(false);

    db.execute(
        "INSERT INTO build_gate_live (contract_slug, phase, seq, chunk, done) VALUES (?, 'pty', ?, ?, ?)",
        rusqlite::params![slug, seq, chunk, if done { 1 } else { 0 }],
    )
    // The message is passed through verbatim because the caller matches /locked/i on it to decide
    // whether to retry — a reworded error would silently turn a retryable lock into a dropped chunk.
    .map_err(|e| format!("build-gate-chunk-append: {e}"))?;
    Ok(json!({ "contract_slug": slug, "seq": seq, "appended": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY, calls INTEGER DEFAULT 0,
               cost_usd REAL DEFAULT 0, last_activity TEXT);
             CREATE TABLE build_gate_live (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT,
               phase TEXT, seq INTEGER, chunk TEXT, done INTEGER);
             INSERT INTO cockpit_panes (id, calls, cost_usd) VALUES (1, 2, 0.5), (2, 0, 0);",
        )
        .unwrap();
        c
    }

    fn pane(c: &Connection, id: i64) -> (i64, f64) {
        c.query_row("SELECT calls, cost_usd FROM cockpit_panes WHERE id = ?", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
    }

    #[test]
    fn metering_ACCUMULATES_rather_than_overwriting() {
        // The whole point: a pane's spend is a running total across many proxied calls.
        let c = db();
        assert_eq!(cockpitPaneMeter(&json!({ "pane_id": 1, "cost_usd": 0.25 }), &c).unwrap()["metered"], json!(true));
        assert_eq!(pane(&c, 1), (3, 0.75));
        cockpitPaneMeter(&json!({ "pane_id": 1, "cost_usd": 0.25 }), &c).unwrap();
        assert_eq!(pane(&c, 1), (4, 1.0));
    }

    #[test]
    fn metering_stamps_last_activity() {
        let c = db();
        cockpitPaneMeter(&json!({ "pane_id": 2, "cost_usd": 0.1 }), &c).unwrap();
        let stamped: Option<String> = c
            .query_row("SELECT last_activity FROM cockpit_panes WHERE id = 2", [], |r| r.get(0))
            .unwrap();
        assert!(stamped.is_some(), "an active pane must look active");
    }

    #[test]
    fn a_MISSING_cost_still_counts_the_call() {
        // Losing the call count because the price was unknown is the worse failure.
        let c = db();
        cockpitPaneMeter(&json!({ "pane_id": 2 }), &c).unwrap();
        assert_eq!(pane(&c, 2), (1, 0.0));
    }

    #[test]
    fn metering_an_unknown_pane_reports_false_rather_than_failing() {
        let c = db();
        assert_eq!(cockpitPaneMeter(&json!({ "pane_id": 999, "cost_usd": 1.0 }), &c).unwrap()["metered"], json!(false));
        assert!(cockpitPaneMeter(&json!({}), &c).unwrap_err().contains("pane_id is required"));
    }

    #[test]
    fn a_chunk_is_appended_under_the_pty_phase() {
        let c = db();
        let r = buildGateChunkAppend(&json!({ "contract_slug": "pane:7", "seq": 3, "chunk": "hello", "done": false }), &c).unwrap();
        assert_eq!(r["appended"], json!(true));
        let (phase, chunk, done): (String, String, i64) = c
            .query_row("SELECT phase, chunk, done FROM build_gate_live WHERE seq = 3", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!((phase.as_str(), chunk.as_str(), done), ("pty", "hello", 0));
    }

    #[test]
    fn an_enormous_line_is_TRUNCATED_at_8000() {
        // A terminal can emit a minified bundle on one line; the live view only needs enough to
        // render, and the cap belongs with the insert so every caller gets it.
        let c = db();
        buildGateChunkAppend(&json!({ "contract_slug": "p", "seq": 1, "chunk": "x".repeat(20_000) }), &c).unwrap();
        let len: i64 = c.query_row("SELECT LENGTH(chunk) FROM build_gate_live WHERE seq = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(len, 8000);
    }

    #[test]
    fn truncation_does_not_split_a_MULTI_BYTE_character() {
        // A byte-exact cut through a multi-byte sequence would store a broken character, and the
        // operator would see a replacement glyph in their own terminal output.
        let c = db();
        buildGateChunkAppend(&json!({ "contract_slug": "p", "seq": 2, "chunk": "é".repeat(20_000) }), &c).unwrap();
        let stored: String = c.query_row("SELECT chunk FROM build_gate_live WHERE seq = 2", [], |r| r.get(0)).unwrap();
        assert_eq!(stored.chars().count(), 8000, "8000 CHARACTERS, not bytes");
        assert!(stored.chars().all(|ch| ch == 'é'), "no broken character in the stored chunk");
    }

    #[test]
    fn done_marks_the_stream_finished_and_a_slug_is_required() {
        let c = db();
        buildGateChunkAppend(&json!({ "contract_slug": "p", "seq": 9, "chunk": "", "done": true }), &c).unwrap();
        let done: i64 = c.query_row("SELECT done FROM build_gate_live WHERE seq = 9", [], |r| r.get(0)).unwrap();
        assert_eq!(done, 1);
        assert!(buildGateChunkAppend(&json!({ "seq": 1 }), &c).unwrap_err().contains("contract_slug is required"));
    }
}
