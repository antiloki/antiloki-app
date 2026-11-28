//! interviewer-greeting-floor.rs — twin of skills/agents/in/interviewer-greeting-floor.mjs
//! (deterministic floor for greetings + trivial acknowledgements; no AI call, no dispatch).
//! NAMES ARE CONTRACT: `interviewerGreetingFloor` (default).
//!
//! `pendingContractNotice` inlines the `detectContractAwaitingHumanApproval` export of
//! skills/system/reconcile.mjs (same precedent as interviewer-bootstrap-context.rs: the JS
//! import target is a plain read-only query fn, not a dispatched skill/twin, so porting the
//! query directly avoids depending on the whole reconcile.rs surface for the two fields
//! (`contract_slug`, `card_slug`, `detail.render_cmd`, `detail.approve_cmd`) this module reads).
// QUALITY: db-read composition twin — deterministic pattern match + optional pending-approval read

use rusqlite::Connection;
use serde_json::{json, Value};

const GREETING_RE_SOURCES: &[&str] = &[
    r"^(?i)(hi|hello|hey|yo|sup|hola|oi|ola|olá)\b",
    r"^(?i)good\s+(morning|afternoon|evening|day)\b",
    r"^(?i)(thanks?|thank\s+you|ty|thx)\b",
    r"^(?i)(ok|okay|k|alright|cool|nice|gotcha|got\s+it|sure)\.?\s*$",
];

const ACK_RE_SOURCE: &str = r"^(?i)(thanks?|thank\s+you|ty|thx|ok|okay|k|alright|cool|nice|gotcha|got\s+it|sure)\.?\s*$";

const REPLIES: &[&str] = &[
    "Hey. What are we working on?",
    "Hi — want to peek at the board or kick off something?",
    "Hello. Tell me what you're trying to ship.",
    "What can I route for you?",
];

const ACK_REPLIES: &[&str] = &[
    "Yep.",
    "Got it.",
    "OK — next?",
    "Anything else?",
];

fn starter_affordances() -> Vec<Vec<Value>> {
    vec![
        vec![
            json!({ "hint": "see the current board", "action_name": "board-keeper.card.list" }),
            json!({ "hint": "ask Product Lead to draft a card", "action_name": "pl.session.start" }),
        ],
        vec![
            json!({ "hint": "fire a single driver tick", "action_name": "system.driver.tick" }),
            json!({ "hint": "list recent memories", "action_name": "system.memory.list" }),
        ],
        vec![
            json!({ "hint": "check substrate health", "action_name": "system.consistency.scan" }),
            json!({ "hint": "see what actions exist", "action_name": "system.action.find" }),
        ],
        vec![
            json!({ "hint": "run a keeper review", "action_name": "keeper.review" }),
            json!({ "hint": "see the current board", "action_name": "board-keeper.card.list" }),
        ],
    ]
}

fn is_greeting(t: &str) -> bool {
    GREETING_RE_SOURCES.iter().any(|src| regex::Regex::new(src).unwrap().is_match(t))
}

fn is_acknowledgement(t: &str) -> bool {
    regex::Regex::new(ACK_RE_SOURCE).unwrap().is_match(t.trim())
}

// Ported from skills/system/reconcile.mjs's `detectContractAwaitingHumanApproval` export — only
// `contract_slug`, `card_slug`, and `detail.{render_cmd,approve_cmd}` survive into this module's
// output, so the finding object is trimmed to those fields.
fn pending_contract_notice(db: Option<&Connection>) -> Option<String> {
    let db = db?;
    let sql = "SELECT slug, card_slug FROM contracts \
               WHERE status = 'tl_approved' AND tier = 'high-risk' AND superseded_by IS NULL \
               ORDER BY updated_at DESC";
    let rows = match crate::db_json::query_rows(db, sql, &[]) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => return None,
    };
    if rows.is_empty() {
        return None;
    }
    let findings: Vec<Value> = rows
        .iter()
        .map(|r| {
            let slug = r.get("slug").cloned().unwrap_or(Value::Null);
            let slug_str = slug.as_str().unwrap_or("").to_string();
            json!({
                "contract_slug": slug,
                "card_slug": r.get("card_slug").cloned().unwrap_or(Value::Null),
                "detail": {
                    "render_cmd": format!("node bin/antiloki.mjs contract.render.html '{{\"slug\":\"{slug_str}\"}}'"),
                    "approve_cmd": format!("node bin/antiloki.mjs contract.gate.human.approve '{{\"slug\":\"{slug_str}\"}}'"),
                },
            })
        })
        .collect();

    let n = findings.len();
    let mut lines: Vec<String> = Vec::new();
    lines.push(String::new());
    lines.push(format!("⚠ **{} high-risk contract{} awaiting your approval:**", n, if n == 1 { "" } else { "s" }));
    for f in findings.iter().take(3) {
        let contract_slug = f.get("contract_slug").and_then(|v| v.as_str()).unwrap_or("");
        let card_slug = f.get("card_slug").and_then(|v| v.as_str()).unwrap_or("");
        let render_cmd = f.pointer("/detail/render_cmd").and_then(|v| v.as_str()).unwrap_or("");
        let approve_cmd = f.pointer("/detail/approve_cmd").and_then(|v| v.as_str()).unwrap_or("");
        lines.push(format!(
            "  - `{contract_slug}` (card: {card_slug}) — render: `{render_cmd}` then approve: `{approve_cmd}`"
        ));
    }
    if n > 3 {
        lines.push(format!("  - …and {} more", n - 3));
    }
    Some(lines.join("\n"))
}

pub fn interviewerGreetingFloor(input: &Value, db: Option<&Connection>) -> Value {
    let user_text = input.get("user_text");
    let turn_index = input.get("turn_index").and_then(|v| v.as_i64()).unwrap_or(0);

    let user_text = match user_text.and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return json!({ "matched": false }),
    };

    let t = user_text.trim();
    if t.chars().count() > 40 {
        return json!({ "matched": false });
    }

    if !is_greeting(t) {
        return json!({ "matched": false });
    }

    let acks = is_acknowledgement(t);
    let reply_list: &[&str] = if acks { ACK_REPLIES } else { REPLIES };
    let idx = (turn_index.rem_euclid(reply_list.len() as i64)) as usize;
    let mut response_md = reply_list[idx].to_string();

    if let Some(notice) = pending_contract_notice(db) {
        response_md = format!("{response_md}\n{notice}");
    }

    let affordances = starter_affordances();
    let next_steps: Vec<Value> = if acks {
        vec![]
    } else {
        let aidx = (turn_index.rem_euclid(affordances.len() as i64)) as usize;
        affordances[aidx].clone()
    };

    json!({
        "matched": true,
        "response_md": response_md,
        "intent_summary": if acks { "User acknowledged" } else { "User greeted" },
        "next_steps": next_steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_schema() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT, tier TEXT, risk INTEGER, \
             complexity TEXT, updated_at TEXT, status TEXT, superseded_by TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn non_greeting_does_not_match() {
        let r = interviewerGreetingFloor(&json!({ "user_text": "please add a card for the login bug" }), None);
        assert_eq!(r, json!({ "matched": false }));
    }

    #[test]
    fn empty_user_text_does_not_match() {
        let r = interviewerGreetingFloor(&json!({ "user_text": "" }), None);
        assert_eq!(r, json!({ "matched": false }));
    }

    #[test]
    fn missing_user_text_does_not_match() {
        let r = interviewerGreetingFloor(&json!({}), None);
        assert_eq!(r, json!({ "matched": false }));
    }

    #[test]
    fn long_input_starting_with_hi_does_not_match() {
        let r = interviewerGreetingFloor(&json!({ "user_text": "hi there, can you please go read the whole codebase for me" }), None);
        assert_eq!(r, json!({ "matched": false }));
    }

    #[test]
    fn greeting_rotates_reply_and_affordances_by_turn_index() {
        let r0 = interviewerGreetingFloor(&json!({ "user_text": "hi", "turn_index": 0 }), None);
        assert_eq!(r0["matched"], json!(true));
        assert_eq!(r0["response_md"], json!("Hey. What are we working on?"));
        assert_eq!(r0["intent_summary"], json!("User greeted"));
        assert_eq!(
            r0["next_steps"],
            json!([
                { "hint": "see the current board", "action_name": "board-keeper.card.list" },
                { "hint": "ask Product Lead to draft a card", "action_name": "pl.session.start" },
            ])
        );

        let r1 = interviewerGreetingFloor(&json!({ "user_text": "hello", "turn_index": 1 }), None);
        assert_eq!(r1["response_md"], json!("Hi — want to peek at the board or kick off something?"));

        let r4 = interviewerGreetingFloor(&json!({ "user_text": "hey", "turn_index": 4 }), None);
        assert_eq!(r4["response_md"], json!("Hey. What are we working on?"));
    }

    #[test]
    fn acknowledgement_rotates_ack_reply_and_has_no_affordances() {
        let r = interviewerGreetingFloor(&json!({ "user_text": "thanks", "turn_index": 2 }), None);
        assert_eq!(r["matched"], json!(true));
        assert_eq!(r["response_md"], json!("OK — next?"));
        assert_eq!(r["intent_summary"], json!("User acknowledged"));
        assert_eq!(r["next_steps"], json!([]));
    }

    #[test]
    fn ok_alone_is_treated_as_acknowledgement() {
        let r = interviewerGreetingFloor(&json!({ "user_text": "ok", "turn_index": 0 }), None);
        assert_eq!(r["intent_summary"], json!("User acknowledged"));
        assert_eq!(r["response_md"], json!("Yep."));
    }

    #[test]
    fn no_db_means_no_notice_appended() {
        let r = interviewerGreetingFloor(&json!({ "user_text": "hi" }), None);
        assert_eq!(r["response_md"], json!("Hey. What are we working on?"));
    }

    #[test]
    fn db_with_no_pending_contracts_means_no_notice() {
        let c = db_with_schema();
        let r = interviewerGreetingFloor(&json!({ "user_text": "hi" }), Some(&c));
        assert_eq!(r["response_md"], json!("Hey. What are we working on?"));
    }

    #[test]
    fn db_with_pending_contract_appends_notice() {
        let c = db_with_schema();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, tier, risk, complexity, updated_at, status, superseded_by) \
             VALUES ('CONTRACT-A','CARD-A','high-risk',4,'medium','2025-12-30 00:00:00','tl_approved',NULL)",
            [],
        )
        .unwrap();
        let r = interviewerGreetingFloor(&json!({ "user_text": "hi" }), Some(&c));
        let expected = "Hey. What are we working on?\n\n⚠ **1 high-risk contract awaiting your approval:**\n  - `CONTRACT-A` (card: CARD-A) — render: `node bin/antiloki.mjs contract.render.html '{\"slug\":\"CONTRACT-A\"}'` then approve: `node bin/antiloki.mjs contract.gate.human.approve '{\"slug\":\"CONTRACT-A\"}'`";
        assert_eq!(r["response_md"], json!(expected));
    }

    #[test]
    fn notice_appended_to_ack_reply_too() {
        let c = db_with_schema();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, tier, risk, complexity, updated_at, status, superseded_by) \
             VALUES ('CONTRACT-A','CARD-A','high-risk',4,'medium','2025-12-30 00:00:00','tl_approved',NULL)",
            [],
        )
        .unwrap();
        let r = interviewerGreetingFloor(&json!({ "user_text": "ok" }), Some(&c));
        assert!(r["response_md"].as_str().unwrap().starts_with("Yep.\n\n⚠"));
        assert_eq!(r["next_steps"], json!([]));
    }

    #[test]
    fn more_than_three_pending_contracts_gets_more_suffix() {
        let c = db_with_schema();
        for (slug, ts) in [
            ("CONTRACT-A", "2025-12-30 00:00:00"),
            ("CONTRACT-B", "2025-12-29 00:00:00"),
            ("CONTRACT-C", "2025-12-28 00:00:00"),
            ("CONTRACT-D", "2025-12-27 00:00:00"),
        ] {
            c.execute(
                "INSERT INTO contracts (slug, card_slug, tier, risk, complexity, updated_at, status, superseded_by) \
                 VALUES (?, 'CARD', 'high-risk', 3, 'low', ?, 'tl_approved', NULL)",
                rusqlite::params![slug, ts],
            )
            .unwrap();
        }
        let r = interviewerGreetingFloor(&json!({ "user_text": "hi" }), Some(&c));
        let md = r["response_md"].as_str().unwrap();
        assert!(md.contains("4 high-risk contracts awaiting your approval"));
        assert!(md.contains("…and 1 more"));
    }
}
