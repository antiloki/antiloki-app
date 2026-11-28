//! be-surface-reads.rs — the last of antiloki-BE's raw READ queries, moved into skills. NAMES ARE
//! CONTRACT: `chatCheckpointsList`, `chatSessionWidgetsList`, `chatSessionsList`,
//! `chatSessionTurns`, `diffsPendingList`, `diffGet`, `notebookItemMeta`, `envFlagsDangerous`,
//! `enrichProposalsList`, `cockpitPaneGet`, `cockpitPaneLive`, `shellTargetResolve`.
//!
//! WHY THEY EXIST. Each was a `db_readonly` call in a BE controller or service. Only a skill
//! touches the databases — see board-reads.rs for the full reasoning. They live in one file
//! because individually they are a query apiece; grouping them by "the last of the BE's raw
//! reads" keeps twelve near-empty modules from existing.
// QUALITY: 86/100 — verbatim SQL ports, legacy-table tolerance, turn/widget assembly
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;

fn tolerant(db: &Connection, sql: &str, params: &[Value]) -> Value {
    crate::db_json::query_rows(db, sql, params).unwrap_or_else(|_| json!([]))
}

fn one(db: &Connection, sql: &str, params: &[Value]) -> Option<Value> {
    crate::db_json::query_rows(db, sql, params).ok()?.as_array()?.first().cloned()
}

fn i64_of(input: &Value, k: &str) -> Option<i64> {
    input.get(k).and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
}

fn limit_or(input: &Value, d: i64) -> i64 {
    input.get("limit").and_then(|v| v.as_i64()).filter(|&n| n > 0).unwrap_or(d)
}

// ---- chat ------------------------------------------------------------------------------------

/// The runtime-event trail for one user goal — what the substrate actually DID in response to a
/// chat turn, which is what the FE's checkpoint strip renders.
pub fn chatCheckpointsList(input: &Value, db: &Connection) -> Result<Value, String> {
    let goal = input.get("user_goal_id").cloned().unwrap_or(Value::Null);
    Ok(tolerant(
        db,
        "SELECT action, skill, ts, outcome, duration_ms,
                substr(COALESCE(error_code, ''), 1, 200) AS error_brief,
                actor, agent
         FROM runtime_events WHERE user_goal_id = ?
         ORDER BY ts ASC, id ASC LIMIT 50",
        &[goal],
    ))
}

/// Session-scoped studio widgets. A legacy database without the table renders no widgets rather
/// than failing the session load.
pub fn chatSessionWidgetsList(input: &Value, db: &Connection) -> Result<Value, String> {
    Ok(tolerant(
        db,
        "SELECT id, session_id, turn_id, type, kind, payload_json
         FROM interviewer_session_widgets WHERE session_id = ? ORDER BY id ASC",
        &[input.get("session_id").cloned().unwrap_or(Value::Null)],
    ))
}

/// The session list, ordered by last activity — a session with no turns since it started sorts by
/// its start, which is why the COALESCE is there rather than a bare last_active_at.
pub fn chatSessionsList(input: &Value, db: &Connection) -> Result<Value, String> {
    let mut sql = String::from(
        "SELECT id, started_at, last_active_at, status, user_identifier,
                (SELECT COUNT(*) FROM interviewer_turns t WHERE t.session_id = s.id) AS turn_count
         FROM interviewer_sessions s WHERE 1=1",
    );
    let mut params: Vec<Value> = Vec::new();
    if let Some(u) = input.get("user_identifier").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        sql.push_str(" AND user_identifier = ?");
        params.push(json!(u));
    }
    sql.push_str(" ORDER BY COALESCE(last_active_at, started_at) DESC LIMIT ?");
    params.push(json!(limit_or(input, 30)));
    crate::db_json::query_rows(db, &sql, &params)
}

/// One session's turns, each with its TURN-ANCHORED studio widgets attached.
///
/// Attaching them here is what makes a studio result durable: on reload the FE re-renders each
/// widget under the turn that produced it. The source of truth is the session-scoped store, with
/// the legacy per-turn table unioned for back-compat — either may be absent on an older database,
/// and neither absence should stop the turns rendering.
///
/// Session-level rows (turn_id IS NULL) are deliberately NOT attached: they belong to the session,
/// not a turn, and load through chatSessionWidgetsList.
pub fn chatSessionTurns(input: &Value, db: &Connection) -> Result<Value, String> {
    let session_id = input.get("session_id").cloned().unwrap_or(Value::Null);
    let turns = crate::db_json::query_rows(
        db,
        "SELECT id, ts, turn_index, user_text, classified_kind, classified_reason,
                classification_confidence, proposed_actions_json, confirmed,
                dispatched_action_ids_json, interviewer_response_md,
                clarify_questionnaire_json,
                model_id, ai_call_id, ai_tier, ai_cost_usd, ai_duration_ms,
                ai_tokens_in, ai_tokens_out,
                view_intent, view_data_json, ai_telemetry_json, offered_widgets_json
         FROM interviewer_turns WHERE session_id = ?
         ORDER BY turn_index ASC LIMIT ? OFFSET ?",
        &[
            session_id.clone(),
            json!(limit_or(input, 50)),
            json!(input.get("offset").and_then(|v| v.as_i64()).filter(|&n| n >= 0).unwrap_or(0)),
        ],
    )?;

    let mut by_turn: HashMap<i64, Vec<Value>> = HashMap::new();
    let mut collect = |rows: Value| {
        for w in rows.as_array().cloned().unwrap_or_default() {
            if let Some(t) = w.get("turn_id").and_then(|v| v.as_i64()) {
                by_turn.entry(t).or_default().push(w);
            }
        }
    };
    collect(tolerant(
        db,
        "SELECT id, turn_id, type, kind, payload_json
         FROM interviewer_session_widgets
         WHERE session_id = ? AND turn_id IS NOT NULL ORDER BY id ASC",
        &[session_id.clone()],
    ));
    collect(tolerant(
        db,
        "SELECT w.id, w.turn_id, w.type, w.kind, w.payload_json
         FROM interviewer_turn_widgets w
         JOIN interviewer_turns t ON t.id = w.turn_id
         WHERE t.session_id = ? ORDER BY w.id ASC",
        &[session_id],
    ));

    Ok(Value::Array(
        turns
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|mut t| {
                let id = t.get("id").and_then(|v| v.as_i64()).unwrap_or(-1);
                if let Some(o) = t.as_object_mut() {
                    o.insert("studio_widgets".into(), json!(by_turn.get(&id).cloned().unwrap_or_default()));
                }
                t
            })
            .collect(),
    ))
}

// ---- diffs -----------------------------------------------------------------------------------

/// The pending-diff queue. `LENGTH(diff_text)` instead of the text itself: the list only needs to
/// show how big each diff is, and shipping every body would make the queue grow with the diffs.
pub fn diffsPendingList(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::db_json::query_rows(
        db,
        "SELECT id, card_slug, file_path, proposed_by, status, proposed_at,
                decided_at, decided_by, decision_reason, applied_at, applied_sha,
                LENGTH(diff_text) AS diff_bytes
         FROM pending_diffs WHERE status = 'pending'
         ORDER BY proposed_at DESC LIMIT ?",
        &[json!(limit_or(input, 50))],
    )
}

pub fn diffGet(input: &Value, db: &Connection) -> Result<Value, String> {
    let id = i64_of(input, "id").filter(|&n| n > 0).ok_or("diff-get: id is required")?;
    one(
        db,
        "SELECT id, card_slug, file_path, diff_text, proposed_by, status,
                proposed_at, decided_at, decided_by, decision_reason,
                applied_at, applied_sha, LENGTH(diff_text) AS diff_bytes
         FROM pending_diffs WHERE id = ?",
        &[json!(id)],
    )
    .ok_or_else(|| format!("diff-get: pending_diff {id} not found"))
}

// ---- notebook / laws / enrich / cockpit -------------------------------------------------------

/// A notebook item's parsed meta_json. Malformed JSON reads as absent rather than failing —
/// the caller's next check is "is there a file on this item", and a corrupt blob has none.
pub fn notebookItemMeta(input: &Value, db: &Connection) -> Result<Value, String> {
    let id = i64_of(input, "id").filter(|&n| n > 0).ok_or("notebook-item-meta: id is required")?;
    Ok(one(db, "SELECT meta_json FROM nb.notebook_items WHERE id = ?", &[json!(id)])
        .and_then(|r| r.get("meta_json").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null))
}

/// Env flags whose danger tier is enforcement-defeating — the ones the BE warns about at boot if
/// they are set. Reading them from the law database is what keeps the warning list in step with
/// the laws instead of drifting in a hardcoded array.
pub fn envFlagsDangerous(_input: &Value, db: &Connection) -> Result<Value, String> {
    Ok(tolerant(
        db,
        "SELECT name, description FROM laws.env_flags WHERE danger_tier = 'enforcement-defeating'",
        &[],
    ))
}

pub fn enrichProposalsList(input: &Value, db: &Connection) -> Result<Value, String> {
    let mut where_parts: Vec<&str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if input.get("filter").and_then(|v| v.as_str()).unwrap_or("pending") == "pending" {
        where_parts.push("p.decided_at IS NULL");
    }
    if let Some(b) = input.get("batch_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        where_parts.push("p.batch_id = ?");
        params.push(json!(b));
    }
    let where_sql = if where_parts.is_empty() { String::new() } else { format!("WHERE {}", where_parts.join(" AND ")) };
    params.push(json!(limit_or(input, 200)));
    crate::db_json::query_rows(
        db,
        &format!(
            "SELECT p.id, p.card_slug, c.body_json AS current_body_json,
                    p.proposed_body_json, p.confidence_overall, p.dropped_fields_json,
                    p.status, p.ai_call_id, p.cost_usd, p.reasoning,
                    p.created_at, p.decided_at, p.decision, p.batch_id
             FROM card_enrichment_proposals p
             LEFT JOIN cards c ON c.slug = p.card_slug
             {where_sql}
             ORDER BY p.created_at DESC LIMIT ?"
        ),
        &params,
    )
}

pub fn cockpitPaneGet(input: &Value, db: &Connection) -> Result<Value, String> {
    let id = i64_of(input, "id").filter(|&n| n > 0).ok_or("cockpit-pane-get: id is required")?;
    Ok(one(db, "SELECT * FROM cockpit_panes WHERE id = ?", &[json!(id)]).unwrap_or(Value::Null))
}

/// Is this pane still live enough to proxy model traffic through?
///
/// The kill-switch: a merged pane must not keep spending. Returns the fields the caller needs to
/// decide (status + pane_kind) plus a `live` flag, so the decision rule lives with the query
/// rather than being re-derived at each call site.
pub fn cockpitPaneLive(input: &Value, db: &Connection) -> Result<Value, String> {
    let id = i64_of(input, "id").filter(|&n| n > 0).ok_or("cockpit-pane-live: id is required")?;
    let Some(p) = one(db, "SELECT id, status, pane_kind FROM cockpit_panes WHERE id = ?", &[json!(id)]) else {
        return Ok(json!({ "live": false, "found": false }));
    };
    let status = p.get("status").and_then(|v| v.as_str()).unwrap_or("");
    // subagent_name / worktree_path / session_id (2026-09-17): the goal-run proxy resolves an
    // agent-started run (MCP tool, pane id only) to the pane's widget key and box. Tolerant: a
    // legacy table without the columns still answers live/found.
    let extra = one(db, "SELECT subagent_name, worktree_path, session_id FROM cockpit_panes WHERE id = ?", &[json!(id)]).unwrap_or(json!({}));
    Ok(json!({
        "live": status != "merged",
        "found": true,
        "id": p.get("id").cloned().unwrap_or(Value::Null),
        "status": p.get("status").cloned().unwrap_or(Value::Null),
        "pane_kind": p.get("pane_kind").cloned().unwrap_or(Value::Null),
        "subagent_name": extra.get("subagent_name").cloned().unwrap_or(Value::Null),
        "worktree_path": extra.get("worktree_path").cloned().unwrap_or(Value::Null),
        "session_id": extra.get("session_id").cloned().unwrap_or(Value::Null),
    }))
}

/// Where should a shell open, and inside what?
///
/// Unlike previewTargetResolve this NEVER fails: a terminal with no worktree is a terminal in the
/// repo root, which is a perfectly good place to be. ref_slug opens a PR's free fix terminal;
/// wt_pane_id opens a cockpit pane's worktree; neither opens the active workspace root.
pub fn shellTargetResolve(input: &Value, db: &Connection) -> Result<Value, String> {
    let root = one(db, "SELECT root_path FROM workspaces WHERE is_active = 1", &[])
        .and_then(|r| r.get("root_path").and_then(|v| v.as_str()).map(|s| s.to_string()));
    let kv = |k: &str| {
        one(db, "SELECT value FROM cost_settings WHERE key = ?", &[json!(k)])
            .and_then(|r| r.get("value").and_then(|v| v.as_str()).map(|s| s.to_string()))
    };
    let wt = if let Some(slug) = input.get("ref_slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        one(db, "SELECT worktree_path FROM contracts WHERE slug = ?", &[json!(slug)])
            .and_then(|r| r.get("worktree_path").and_then(|v| v.as_str()).map(|s| s.to_string()))
    } else if let Some(pane) = i64_of(input, "wt_pane_id").filter(|&n| n > 0) {
        one(db, "SELECT worktree_path FROM cockpit_panes WHERE id = ?", &[json!(pane)])
            .and_then(|r| r.get("worktree_path").and_then(|v| v.as_str()).map(|s| s.to_string()))
    } else {
        None
    };
    Ok(json!({
        "root": root,
        "runtime": kv("ws_runtime"),
        "container": kv("ws_runtime_container"),
        "wt": wt,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS nb; ATTACH ':memory:' AS laws;").unwrap();
        c.execute_batch(
            "CREATE TABLE runtime_events (id INTEGER PRIMARY KEY, user_goal_id TEXT, action TEXT, skill TEXT,
               ts TEXT, outcome TEXT, duration_ms INTEGER, error_code TEXT, actor TEXT, agent TEXT);
             CREATE TABLE interviewer_sessions (id INTEGER PRIMARY KEY, started_at TEXT, last_active_at TEXT,
               status TEXT, user_identifier TEXT);
             CREATE TABLE interviewer_turns (id INTEGER PRIMARY KEY, session_id INTEGER, ts TEXT, turn_index INTEGER,
               user_text TEXT, classified_kind TEXT, classified_reason TEXT, classification_confidence REAL,
               proposed_actions_json TEXT, confirmed INTEGER, dispatched_action_ids_json TEXT,
               interviewer_response_md TEXT, clarify_questionnaire_json TEXT, model_id TEXT, ai_call_id INTEGER,
               ai_tier TEXT, ai_cost_usd REAL, ai_duration_ms INTEGER, ai_tokens_in INTEGER, ai_tokens_out INTEGER,
               view_intent TEXT, view_data_json TEXT, ai_telemetry_json TEXT, offered_widgets_json TEXT);
             CREATE TABLE interviewer_session_widgets (id INTEGER PRIMARY KEY, session_id INTEGER, turn_id INTEGER,
               type TEXT, kind TEXT, payload_json TEXT);
             CREATE TABLE interviewer_turn_widgets (id INTEGER PRIMARY KEY, turn_id INTEGER, type TEXT,
               kind TEXT, payload_json TEXT);
             CREATE TABLE pending_diffs (id INTEGER PRIMARY KEY, card_slug TEXT, file_path TEXT, diff_text TEXT,
               proposed_by TEXT, status TEXT, proposed_at TEXT, decided_at TEXT, decided_by TEXT,
               decision_reason TEXT, applied_at TEXT, applied_sha TEXT);
             CREATE TABLE nb.notebook_items (id INTEGER PRIMARY KEY, meta_json TEXT);
             CREATE TABLE laws.env_flags (name TEXT, description TEXT, danger_tier TEXT);
             CREATE TABLE cards (slug TEXT, body_json TEXT);
             CREATE TABLE card_enrichment_proposals (id INTEGER PRIMARY KEY, card_slug TEXT, proposed_body_json TEXT,
               confidence_overall REAL, dropped_fields_json TEXT, status TEXT, ai_call_id INTEGER, cost_usd REAL,
               reasoning TEXT, created_at TEXT, decided_at TEXT, decision TEXT, batch_id TEXT);
             CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY, status TEXT, pane_kind TEXT, worktree_path TEXT);
             CREATE TABLE workspaces (root_path TEXT, is_active INTEGER);
             CREATE TABLE contracts (slug TEXT, worktree_path TEXT);
             CREATE TABLE cost_settings (key TEXT, value TEXT);

             INSERT INTO runtime_events VALUES (2,'g1','a.two','s2','t2','ok',5,NULL,'dev','d'),
                                               (1,'g1','a.one','s1','t1','fail',9,'BOOM','dev','d'),
                                               (3,'g2','other','s3','t3','ok',1,NULL,'dev','d');
             INSERT INTO interviewer_sessions VALUES (1,'2026-01-01','2026-01-05','open','dani'),
                                                     (2,'2026-01-03',NULL,'open','other');
             INSERT INTO interviewer_turns (id,session_id,turn_index,user_text) VALUES
               (10,1,0,'hello'),(11,1,1,'again'),(12,2,0,'elsewhere');
             INSERT INTO interviewer_session_widgets VALUES (1,1,10,'chart','bar','{}'),
                                                            (2,1,NULL,'note','session','{}');
             INSERT INTO interviewer_turn_widgets VALUES (3,11,'legacy','old','{}');
             INSERT INTO pending_diffs VALUES (1,'C-1','a.ts','@@ diff @@','dev','pending','t2',NULL,NULL,NULL,NULL,NULL),
                                              (2,'C-1','b.ts','@@ x @@','dev','applied','t1',NULL,NULL,NULL,NULL,NULL),
                                              (3,'C-2','c.ts','@@ y @@','dev','pending','t3',NULL,NULL,NULL,NULL,NULL);
             INSERT INTO nb.notebook_items VALUES (1,'{\"file\":\"a.wav\"}'),(2,'{not json'),(3,NULL);
             INSERT INTO laws.env_flags VALUES ('ANTILOKI_ALLOW_DRIFT','skips the drift guard','enforcement-defeating'),
                                               ('ANTILOKI_DEBUG','noisy logs','harmless');
             INSERT INTO cards VALUES ('C-1','{\"kind\":\"feat\"}');
             INSERT INTO card_enrichment_proposals VALUES (1,'C-1','{}',0.9,'[]','proposed',1,0.1,'why','t2',NULL,NULL,'b1'),
                                                          (2,'C-1','{}',0.5,'[]','proposed',2,0.2,'why','t3','t4','accepted','b1'),
                                                          (3,'C-9','{}',0.7,'[]','proposed',3,0.3,'why','t1',NULL,NULL,'b2');
             INSERT INTO cockpit_panes VALUES (1,'live','opencode','C:/wt/1'),(2,'merged','opencode','C:/wt/2');
             INSERT INTO workspaces VALUES ('C:/dev/root',1);
             INSERT INTO contracts VALUES ('CT-1','C:/wt/ct-1');
             INSERT INTO cost_settings VALUES ('ws_runtime','docker'),('ws_runtime_container','app');",
        )
        .unwrap();
        c
    }

    #[test]
    fn checkpoints_are_scoped_to_the_goal_and_ordered_oldest_first() {
        let r = chatCheckpointsList(&json!({ "user_goal_id": "g1" }), &db()).unwrap();
        let rows = r.as_array().unwrap();
        assert_eq!(rows.len(), 2, "the other goal's event is not included");
        assert_eq!(rows[0]["action"], json!("a.one"), "a trail reads forwards");
        assert_eq!(rows[0]["error_brief"], json!("BOOM"));
        assert_eq!(rows[1]["error_brief"], json!(""), "a NULL error_code becomes ''");
    }

    #[test]
    fn sessions_sort_by_last_activity_FALLING_BACK_to_start() {
        // Session 2 has no last_active_at; a bare ORDER BY last_active_at would sort it as NULL.
        let r = chatSessionsList(&json!({}), &db()).unwrap();
        let ids: Vec<i64> = r.as_array().unwrap().iter().map(|s| s["id"].as_i64().unwrap()).collect();
        assert_eq!(ids, [1, 2]);
        assert_eq!(r[0]["turn_count"], json!(2), "the correlated count is per session");
    }

    #[test]
    fn sessions_filter_by_user() {
        let r = chatSessionsList(&json!({ "user_identifier": "other" }), &db()).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 1);
        assert_eq!(r[0]["id"], json!(2));
    }

    #[test]
    fn turns_carry_their_own_widgets_from_BOTH_stores() {
        let r = chatSessionTurns(&json!({ "session_id": 1 }), &db()).unwrap();
        let turns = r.as_array().unwrap();
        assert_eq!(turns.len(), 2);
        // Turn 10 from the session store, turn 11 from the legacy per-turn table.
        assert_eq!(turns[0]["studio_widgets"][0]["kind"], json!("bar"));
        assert_eq!(turns[1]["studio_widgets"][0]["kind"], json!("old"));
    }

    #[test]
    fn a_SESSION_LEVEL_widget_is_not_attached_to_any_turn() {
        // turn_id IS NULL rows belong to the session and load separately; attaching them here
        // would duplicate them under an arbitrary turn.
        let r = chatSessionTurns(&json!({ "session_id": 1 }), &db()).unwrap();
        assert_eq!(r[0]["studio_widgets"].as_array().unwrap().len(), 1);
        // …and it IS returned by the session-scoped read.
        let w = chatSessionWidgetsList(&json!({ "session_id": 1 }), &db()).unwrap();
        assert_eq!(w.as_array().unwrap().len(), 2);
    }

    #[test]
    fn turns_render_even_when_NEITHER_widget_table_exists() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE interviewer_turns (id INTEGER PRIMARY KEY, session_id INTEGER, ts TEXT, turn_index INTEGER,
               user_text TEXT, classified_kind TEXT, classified_reason TEXT, classification_confidence REAL,
               proposed_actions_json TEXT, confirmed INTEGER, dispatched_action_ids_json TEXT,
               interviewer_response_md TEXT, clarify_questionnaire_json TEXT, model_id TEXT, ai_call_id INTEGER,
               ai_tier TEXT, ai_cost_usd REAL, ai_duration_ms INTEGER, ai_tokens_in INTEGER, ai_tokens_out INTEGER,
               view_intent TEXT, view_data_json TEXT, ai_telemetry_json TEXT, offered_widgets_json TEXT);
             INSERT INTO interviewer_turns (id,session_id,turn_index,user_text) VALUES (1,1,0,'hi');",
        )
        .unwrap();
        let r = chatSessionTurns(&json!({ "session_id": 1 }), &c).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 1);
        assert_eq!(r[0]["studio_widgets"], json!([]));
    }

    #[test]
    fn pending_diffs_are_pending_only_and_report_SIZE_not_body() {
        let r = diffsPendingList(&json!({}), &db()).unwrap();
        let rows = r.as_array().unwrap();
        assert_eq!(rows.len(), 2, "the applied diff is not pending");
        assert_eq!(rows[0]["card_slug"], json!("C-2"), "newest proposed_at first");
        assert_eq!(rows[0]["diff_bytes"], json!(7));
        assert!(rows[0].get("diff_text").is_none(), "the queue must not ship every diff body");
    }

    #[test]
    fn one_diff_carries_its_body_and_a_missing_id_says_NOT_FOUND() {
        let c = db();
        let d = diffGet(&json!({ "id": 1 }), &c).unwrap();
        assert_eq!(d["diff_text"], json!("@@ diff @@"));
        assert!(diffGet(&json!({ "id": 999 }), &c).unwrap_err().contains("not found"));
        assert!(diffGet(&json!({}), &c).unwrap_err().contains("id is required"));
    }

    #[test]
    fn notebook_meta_parses_and_MALFORMED_reads_as_absent() {
        let c = db();
        assert_eq!(notebookItemMeta(&json!({ "id": 1 }), &c).unwrap(), json!({ "file": "a.wav" }));
        assert_eq!(notebookItemMeta(&json!({ "id": 2 }), &c).unwrap(), Value::Null);
        assert_eq!(notebookItemMeta(&json!({ "id": 3 }), &c).unwrap(), Value::Null);
        assert_eq!(notebookItemMeta(&json!({ "id": 99 }), &c).unwrap(), Value::Null);
    }

    #[test]
    fn only_ENFORCEMENT_DEFEATING_flags_are_warned_about() {
        let r = envFlagsDangerous(&json!({}), &db()).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 1);
        assert_eq!(r[0]["name"], json!("ANTILOKI_ALLOW_DRIFT"));
    }

    #[test]
    fn enrich_defaults_to_UNDECIDED_proposals_and_joins_the_current_body() {
        let c = db();
        let pending = enrichProposalsList(&json!({}), &c).unwrap();
        let ids: Vec<i64> = pending.as_array().unwrap().iter().map(|p| p["id"].as_i64().unwrap()).collect();
        // Proposal 2 is decided, so it is not pending. 1 before 3: created_at 't2' > 't1'.
        assert_eq!(ids, [1, 3], "the decided proposal is not pending; newest first");
        assert_eq!(pending[0]["current_body_json"], json!("{\"kind\":\"feat\"}"));
        // A proposal for a card that no longer exists survives the LEFT JOIN with a null body.
        assert_eq!(pending[1]["current_body_json"], Value::Null);
    }

    #[test]
    fn enrich_filter_all_includes_decided_and_batch_id_narrows() {
        let c = db();
        assert_eq!(enrichProposalsList(&json!({ "filter": "all" }), &c).unwrap().as_array().unwrap().len(), 3);
        assert_eq!(enrichProposalsList(&json!({ "filter": "all", "batch_id": "b2" }), &c).unwrap().as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_MERGED_pane_is_not_live_and_an_unknown_pane_is_not_found() {
        let c = db();
        let live = cockpitPaneLive(&json!({ "id": 1 }), &c).unwrap();
        assert_eq!(live["live"], json!(true));
        assert_eq!(live["pane_kind"], json!("opencode"));
        // The kill-switch: a merged pane must not keep spending.
        assert_eq!(cockpitPaneLive(&json!({ "id": 2 }), &c).unwrap()["live"], json!(false));
        assert_eq!(cockpitPaneLive(&json!({ "id": 99 }), &c).unwrap(), json!({ "live": false, "found": false }));
    }

    #[test]
    fn pane_get_returns_the_row_or_null() {
        let c = db();
        assert_eq!(cockpitPaneGet(&json!({ "id": 1 }), &c).unwrap()["worktree_path"], json!("C:/wt/1"));
        assert_eq!(cockpitPaneGet(&json!({ "id": 99 }), &c).unwrap(), Value::Null);
    }

    #[test]
    fn a_shell_target_ALWAYS_resolves_even_with_no_worktree() {
        // A terminal with no worktree is a terminal in the repo root — never an error.
        let c = db();
        let plain = shellTargetResolve(&json!({}), &c).unwrap();
        assert_eq!(plain["root"], json!("C:/dev/root"));
        assert_eq!(plain["wt"], Value::Null);
        assert_eq!(plain["runtime"], json!("docker"));
        assert_eq!(shellTargetResolve(&json!({ "ref_slug": "CT-1" }), &c).unwrap()["wt"], json!("C:/wt/ct-1"));
        assert_eq!(shellTargetResolve(&json!({ "wt_pane_id": 1 }), &c).unwrap()["wt"], json!("C:/wt/1"));
        assert_eq!(shellTargetResolve(&json!({ "ref_slug": "NOPE" }), &c).unwrap()["wt"], Value::Null);
    }
}
