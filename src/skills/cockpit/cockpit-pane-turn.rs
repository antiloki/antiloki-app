//! cockpit-pane-turn.rs — twin of skills/cockpit/cockpit-pane-turn.mjs. NAMES ARE CONTRACT:
//! `cockpitPaneTurn` (default export). EPIC-VIBE-CHAT-UNIFICATION M1 (session bridge) — a vibe
//! pane message becomes a REAL interviewer chat turn: same engine loop, tool belt, widgets,
//! lifecycle trace and audit path as the operator's chat. The pane is the skin: this skill CALLS
//! `interviewer-turn` with a pane-scoped `user_identifier` ('pane:<id>') and a `pane_overlay`.
//!
//! Legacy vibe capabilities preserved on this path: 🧑/🤖 blackboard thread notes, @mention
//! routing to sibling panes, the per-pane cost meter, the pane's pinned model, status/last_activity.
//!
//! PARITY-DEFERRED (disclosed, not silently dropped): `interviewerTurn`
//! (skills/agents/in/interviewer-turn.mjs) has NO Rust twin yet in this wave — the eventual real
//! call site is `crate::agents_in_interviewer_turn::interviewerTurn`. Per playbook rule 11 ("if
//! the JS imports a skill that is NOT yet a twin, stop / defer that part"), this twin ports the
//! FULL orchestration around that call — the required-field guards, the pane lookup + pane_kind
//! gate, the status/blackboard bookkeeping, `buildPaneOverlay` (persona + scope + mission +
//! coordination + in-scope files, byte-faithful and independently reachable/tested), the
//! read→write follow-up hop loop, the reply derivation, the @mention handoff, the cost meter, and
//! the final response shape — and stops at the exact dispatch point with a clear PARITY-DEFERRED
//! `Err`, which flows through the SAME `try/catch → agent_error` path the JS uses for any
//! `interviewerTurn` failure (mirrors cockpit-pane-message.rs's `aiCall`/`vibeChatOn` boundaries).
//! Once `interviewer-turn.mjs` lands its own Rust twin, `run_turn` below is the one function to
//! repoint at the real `crate::agents_in_interviewer_turn::interviewerTurn` call.
use rusqlite::Connection;
use serde_json::{json, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } }
fn truthy_num(n: f64) -> bool { n != 0.0 && !n.is_nan() }
fn num_param(n: f64) -> Value { if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) } }
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }

// JSON.parse(s ?? '[]') catch → [] ; non-array → []
fn parse_arr(s: Option<&Value>) -> Vec<String> {
    let raw = match s { Some(Value::String(t)) => t.clone(), None | Some(Value::Null) => "[]".to_string(), Some(o) => o.to_string() };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Array(a)) => a.iter().map(|v| str_qq(Some(v))).collect(),
        _ => Vec::new(),
    }
}

fn insert_blackboard(db: &Connection, session_id: &Value, pane_id: &Value, kind: &str, body_md: &str) {
    let _ = crate::db_json::run_write(db, "INSERT INTO cockpit_blackboard (session_id, pane_id, kind, body_md) VALUES (?,?,?,?)", &[session_id.clone(), pane_id.clone(), json!(kind), json!(body_md)]);
}

/// M2 pane overlay — the SAME identity/coordination assembly the legacy loop used (persona = base
/// agent + operator extra guidance; mission carries the 🧠 context pane docs; coordination = the
/// shared blackboard log). Rides into the pane prompt variant via interviewer-turn → interviewer-chat.
/// `async function buildPaneOverlay(pane, context)` — no async boundary needed in the db lane.
fn build_pane_overlay(pane: &Value, db: &Connection) -> Value {
    let base_agent = pane.get("base_agent").and_then(Value::as_str).unwrap_or("");
    let mut persona = String::new();
    if !base_agent.is_empty() {
        // JS `try { ... } catch { /* agents table absent / unknown base → fall back */ }`
        let r = crate::entities_agent_persona_resolve::resolvePersona(db, base_agent);
        if let Some(sm) = r.get("system_md").and_then(Value::as_str) {
            if !sm.is_empty() { persona = sm.to_string(); }
        }
    }
    let extra = str_qq(pane.get("extra_prompt_md"));
    if !extra.trim().is_empty() {
        persona = if persona.is_empty() { format!("OPERATOR EXTRA GUIDANCE:\n{}", extra.trim()) } else { format!("{persona}\n\nOPERATOR EXTRA GUIDANCE:\n{}", extra.trim()) };
    }
    if persona.is_empty() { persona = "(a focused implementation agent working within its scope)".to_string(); }

    let session_id = pane.get("session_id").cloned().unwrap_or(Value::Null);
    let pane_id = pane.get("id").cloned().unwrap_or(Value::Null);
    // JS `try { ... } catch { /* blackboard read is best-effort — the turn still runs */ }`
    let slices = crate::cockpit_cockpit_blackboard_read::cockpitBlackboardRead(&json!({ "session_id": session_id, "pane_id": pane_id }), db).unwrap_or(json!({}));
    let context_docs_md: Vec<String> = slices.get("context_docs").and_then(Value::as_array).cloned().unwrap_or_default().iter()
        .map(|d| format!("--- CONTEXT DOC #{} ---\n{}", str_qq(d.get("id")), str_qq(d.get("body_md"))))
        .collect();
    let mut mission_parts: Vec<String> = vec![str_qq(slices.get("mission_md"))];
    mission_parts.extend(context_docs_md);
    let mission_md_joined = mission_parts.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n");
    let mission_md = if mission_md_joined.is_empty() { "(none)".to_string() } else { mission_md_joined };
    let coordination_joined = slices.get("coordination").and_then(Value::as_array).cloned().unwrap_or_default().iter()
        .map(|c| format!("- [{}] {}", str_qq(c.get("kind")), str_qq(c.get("body_md"))))
        .collect::<Vec<_>>().join("\n");
    let coordination_joined = take_chars(&coordination_joined, 8000);
    let coordination = if coordination_joined.is_empty() { "(none)".to_string() } else { coordination_joined };

    // CURRENT IN-SCOPE FILES ride in the prompt exactly like the legacy loop — the model edits
    // what it can already SEE instead of asking to read first.
    let scope = parse_arr(pane.get("scope_json"));
    let worktree = pane.get("worktree_path").and_then(Value::as_str).filter(|s| !s.is_empty());
    let pane_files = match worktree {
        // JS `try { ... } catch { /* best-effort */ }` — gatherScopedFiles never panics here.
        Some(w) => crate::cockpit_cockpit_pane_message::gatherScopedFiles(&json!(w), &json!(scope), &Value::Null),
        None => json!("(worktree not provisioned)"),
    };

    // pane_id rides in the overlay so interviewer-turn can PIN it onto any cockpit.pane.file.*
    // dispatch (M3 identity enforcement).
    // CONTEXT PACK (2026-09-14): the baked pack rides in the overlay for the chat-engine path too.
    let context_pack = worktree.and_then(|w| crate::cockpit_cockpit_pane_context_bake::pack_prompt_text(w, crate::cockpit_cockpit_pane_context_bake::PACK_PROMPT_CAP)).map(|t| json!(t)).unwrap_or(Value::Null);

    json!({
        "pane_id": pane.get("id").cloned().unwrap_or(Value::Null),
        "persona": persona,
        "scope": scope,
        "mission_md": mission_md,
        "coordination": coordination,
        "pane_files": pane_files,
        "context_pack": context_pack,
        "base_agent": pane.get("base_agent").cloned().unwrap_or(Value::Null),
    })
}

/// PARITY-DEFERRED boundary — see module doc. Mirrors the JS `runTurn` closure's call shape
/// (`interviewerTurn({ user_text, user_identifier, references, attachments, model_id, pane_overlay
/// }, context)`) so the eventual real wiring is a one-line swap once
/// `crate::agents_in_interviewer_turn::interviewerTurn` exists.
fn run_turn(pane_id: i64, pane: &Value, overlay: &Value, input: &Value, text: &str) -> Result<Value, String> {
    let _call = json!({
        "user_text": text,
        "user_identifier": format!("pane:{pane_id}"),
        "references": input.get("references").filter(|v| v.is_array()).cloned().unwrap_or(json!([])),
        "attachments": input.get("attachments").filter(|v| v.is_array()).cloned().unwrap_or(json!([])),
        "model_id": pane.get("model_id").and_then(Value::as_str).filter(|s| !s.is_empty()),
        "pane_overlay": overlay,
    });
    Err("cockpit-pane-turn: interviewerTurn (crate::agents_in_interviewer_turn::interviewerTurn) is PARITY-DEFERRED — skills/agents/in/interviewer-turn.mjs has no Rust twin yet.".into())
}

pub fn cockpitPaneTurn(input: &Value, db: &Connection) -> Result<Value, String> {
    // context.db guard is not reachable here (db always present in the db lane).
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));
    let pane_id_n = js_number(input.get("pane_id"));
    if !truthy_num(pane_id_n) { return Err("cockpit-pane-turn: pane_id is required".into()); }
    let pane_id = num_param(pane_id_n);

    let rows = crate::db_json::query_rows(db, "SELECT * FROM cockpit_panes WHERE id = ?", &[pane_id.clone()])?;
    let pane = match rows.as_array().and_then(|a| a.first()) {
        Some(p) => p.clone(),
        None => return Ok(json!({ "ok": false, "error": format!("pane {} not found", pane_id_n as i64) })),
    };
    let pane_kind = pane.get("pane_kind").and_then(Value::as_str).unwrap_or("agent");
    if pane_kind != "agent" {
        return Ok(json!({ "ok": false, "error": format!("pane {} is a '{}' pane — only agent panes chat", pane_id_n as i64, pane_kind) }));
    }

    let user_text_raw = match input.get("user_input").filter(|v| !v.is_null()) {
        Some(v) => v.clone(),
        None => input.get("user_text").cloned().unwrap_or(Value::Null),
    };
    let user_text = take_chars(&str_qq(Some(&user_text_raw)), 20000);
    if user_text.is_empty() { return Err("cockpit-pane-turn: user_input is required".into()); }

    let session_id = pane.get("session_id").cloned().unwrap_or(Value::Null);
    crate::db_json::run_write(db, "UPDATE cockpit_panes SET status = 'working', last_activity = datetime('now') WHERE id = ?", &[pane_id.clone()])?;
    insert_blackboard(db, &session_id, &pane_id, "note", &format!("🧑 {user_text}"));

    // M2: persona/scope/mission/coordination + in-scope files + grant-filtered catalog. Built once
    // per message — the follow-up hops only READ, so the snapshot stays true.
    let overlay = build_pane_overlay(&pane, db);

    let mut turn: Option<Value> = None;
    let mut agent_error: Option<String> = None;
    let attempt = || -> Result<Value, String> {
        let mut t = run_turn(pane_id_n as i64, &pane, &overlay, input, &user_text)?;
        // READ→WRITE FOLLOW-UP (vibe test 3): the turn loop is one-shot — a turn whose ONLY
        // outcome was file READS would strand the user ("I'll read first" … end). Feed the read
        // content straight back in a bounded continuation so the SAME ask finishes with a write
        // (or an honest answer). Max 2 hops; each is a real turn.
        let mut hops = 0;
        while !truthy(t.get("agent_error")) && hops < 2 {
            let dr: Vec<Value> = t.get("dispatch_results").and_then(Value::as_array).cloned().unwrap_or_default();
            let reads: Vec<&Value> = dr.iter().filter(|d| truthy(d.get("ok")) && d.get("action").and_then(Value::as_str) == Some("cockpit.pane.file.read")).collect();
            let wrote_or_planned = dr.iter().any(|d| d.get("action").and_then(Value::as_str) == Some("cockpit.pane.file.write"))
                || !t.get("clarify_questionnaire").map_or(true, Value::is_null);
            if reads.is_empty() || wrote_or_planned { break; }
            hops += 1;
            let blocks = reads.iter().map(|d| {
                let res = d.get("result").and_then(|r| r.get("result")).or_else(|| d.get("result")).cloned().unwrap_or(json!({}));
                let path = res.get("path").filter(|v| !v.is_null()).map(|p| str_qq(Some(p))).unwrap_or_else(|| "?".to_string());
                let content = match res.get("content").filter(|v| !v.is_null()) {
                    Some(v) => take_chars(&str_qq(Some(v)), 24000),
                    None => "(missing)".to_string(),
                };
                format!("--- {path} (current content) ---\n{content}")
            }).collect::<Vec<_>>().join("\n\n");
            let followup = format!(
                "(follow-up of my previous message — the file content you asked to read is below; now FINISH the task by emitting cockpit.pane.file.write with the FULL new content, or answer if no change is needed)\n\n{blocks}"
            );
            t = run_turn(pane_id_n as i64, &pane, &overlay, input, &followup)?;
        }
        Ok(t)
    };
    // `try { turn = await runTurn(userText); ... } catch (e) { agentError = ...; }` — on error the
    // JS's `turn` binding retains whatever the last successful assignment was (possibly still null
    // on the very first hop, since the failure here is always the deferred first call).
    match attempt() {
        Ok(t) => turn = Some(t),
        Err(e) => agent_error = Some(take_chars(&e, 300)),
    }

    let pres = turn.as_ref().and_then(|t| t.get("presentation_md"));
    let resp = turn.as_ref().and_then(|t| t.get("response_md"));
    let reply_raw = if truthy(pres) {
        pres.cloned().unwrap()
    } else if truthy(resp) {
        resp.cloned().unwrap()
    } else if let Some(ae) = &agent_error {
        json!(format!("agent error: {ae}"))
    } else {
        json!("")
    };
    let reply = str_qq(Some(&reply_raw)).trim().to_string();
    if !reply.is_empty() { insert_blackboard(db, &session_id, &pane_id, "note", &format!("🤖 {}", take_chars(&reply, 4000))); }

    // @mention handoff — identical behavior to the legacy loop (route to the named sibling).
    let mention_re = regex::Regex::new(r"@([A-Za-z0-9_-]+)").unwrap();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for c in mention_re.captures_iter(&user_text) {
        let m = c[1].to_string();
        if !seen.insert(m.clone()) { continue; }
        let target = crate::db_json::query_rows(
            db,
            "SELECT id FROM cockpit_panes WHERE session_id = ? AND (subagent_name = ? OR base_agent = ?) AND id != ?",
            &[session_id.clone(), json!(m), json!(m), pane_id.clone()],
        )?;
        if let Some(t) = target.as_array().and_then(|a| a.first()) {
            let tid = t.get("id").cloned().unwrap_or(Value::Null);
            insert_blackboard(db, &session_id, &tid, "handoff", &format!("↩ from {}: {}", str_qq(pane.get("subagent_name")), take_chars(&user_text, 300)));
            insert_blackboard(db, &session_id, &Value::Null, "handoff", &format!("{} → @{m}", str_qq(pane.get("subagent_name"))));
        }
    }

    // Cost meter: sum every AI call the turn made (triage + inline capabilities + presenter ride
    // in ai_telemetry); the turn row's ai_cost_usd is the fallback.
    let mut cost = 0.0f64;
    let cost_result: Result<f64, String> = (|| {
        let mut c = 0.0f64;
        if let Some(t) = &turn {
            let tel = match t.get("ai_telemetry") {
                Some(Value::Array(a)) => a.clone(),
                Some(v) if !v.is_null() => vec![v.clone()],
                _ => Vec::new(),
            };
            c = tel.iter().map(|x| { let n = js_number(x.get("cost_usd")); if n.is_nan() { 0.0 } else { n } }).sum();
            if c == 0.0 {
                if let Some(tid) = t.get("turn_id").filter(|v| !v.is_null()) {
                    let rows = crate::db_json::query_rows(db, "SELECT ai_cost_usd FROM interviewer_turns WHERE id = ?", &[tid.clone()])?;
                    let v = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("ai_cost_usd")).and_then(Value::as_f64).unwrap_or(0.0);
                    c = v;
                }
            }
        }
        Ok(c)
    })();
    if let Ok(c) = cost_result { cost = c; } // JS `catch { cost = 0; }` on failure — Err() falls through to the 0.0 default above

    let chat_session_id = turn.as_ref().and_then(|t| t.get("session_id")).filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null);
    crate::db_json::run_write(
        db,
        "UPDATE cockpit_panes SET status = ?, last_activity = datetime('now'), calls = calls + 1, cost_usd = cost_usd + ?, chat_session_id = COALESCE(?, chat_session_id) WHERE id = ?",
        &[json!(if agent_error.is_some() { "error" } else { "idle" }), json!(cost), chat_session_id, pane_id.clone()],
    )?;

    Ok(json!({
        "ok": agent_error.is_none(),
        "pane_id": pane_id_n as i64,
        "turn_id": turn.as_ref().and_then(|t| t.get("turn_id")).cloned().unwrap_or(Value::Null),
        "chat_session_id": turn.as_ref().and_then(|t| t.get("session_id")).cloned().unwrap_or(Value::Null),
        "reply_md": reply,
        "gate": turn.as_ref().and_then(|t| t.get("gate")).cloned().unwrap_or(Value::Null),
        "auto_dispatched": turn.as_ref().map_or(false, |t| truthy(t.get("auto_dispatched"))),
        "dispatch_results": turn.as_ref().and_then(|t| t.get("dispatch_results")).cloned().unwrap_or(Value::Null),
        "view_intent": turn.as_ref().and_then(|t| t.get("view_intent")).cloned().unwrap_or(Value::Null),
        "clarify_questionnaire": turn.as_ref().and_then(|t| t.get("clarify_questionnaire")).cloned().unwrap_or(Value::Null),
        "agent_error": agent_error,
        // legacy-shape compat: the current FE reads these off the pane-message result
        "applied": Vec::<Value>::new(),
        "rejected": Vec::<Value>::new(),
        "pending": Value::Null,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c.execute_batch("CREATE TABLE interviewer_turns (id INTEGER PRIMARY KEY, ai_cost_usd REAL);").unwrap();
        c.execute(
            "INSERT INTO cockpit_sessions (id, name, created_at) VALUES (1, 's1', datetime('now'))",
            [],
        ).unwrap();
        c
    }

    fn insert_pane(c: &Connection, name: &str, worktree: &str, scope: &str, pane_kind: &str) -> i64 {
        // idx must be unique per session_id (UNIQUE(session_id, idx)) — derive it from the
        // current pane count so multi-pane tests (e.g. sibling handoff) don't collide on idx=0.
        let idx: i64 = c.query_row("SELECT COUNT(*) FROM cockpit_panes WHERE session_id = 1", [], |r| r.get(0)).unwrap_or(0);
        crate::db_json::run_write(
            c,
            "INSERT INTO cockpit_panes (session_id, idx, subagent_name, worktree_path, scope_json, pane_kind) VALUES (1, ?, ?, ?, ?, ?)",
            &[json!(idx), json!(name), json!(worktree), json!(scope), json!(pane_kind)],
        ).unwrap().1
    }

    #[test]
    fn pane_id_required_errs() {
        let c = seeded();
        let r = cockpitPaneTurn(&json!({}), &c);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("pane_id is required"));
    }

    #[test]
    fn user_input_required_errs() {
        let c = seeded();
        let pane_id = insert_pane(&c, "dev", "", "[]", "agent");
        let r = cockpitPaneTurn(&json!({ "pane_id": pane_id }), &c);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("user_input is required"));
    }

    #[test]
    fn pane_not_found_returns_ok_false() {
        let c = seeded();
        let r = cockpitPaneTurn(&json!({ "pane_id": 999, "user_input": "hi" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "pane 999 not found" }));
    }

    #[test]
    fn non_agent_pane_rejected() {
        let c = seeded();
        let pane_id = insert_pane(&c, "dev", "", "[]", "script");
        let r = cockpitPaneTurn(&json!({ "pane_id": pane_id, "user_input": "hi" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": format!("pane {pane_id} is a 'script' pane — only agent panes chat") }));
    }

    // ── deferred interviewerTurn boundary: everything up to the dispatch is real + reachable ──
    #[test]
    fn deferred_call_sets_agent_error_and_posts_blackboard_notes() {
        let c = seeded();
        let pane_id = insert_pane(&c, "dev", "", "[]", "agent");
        let r = cockpitPaneTurn(&json!({ "pane_id": pane_id, "user_input": "add a feature" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["pane_id"], json!(pane_id));
        assert_eq!(r["turn_id"], Value::Null);
        assert!(r["reply_md"].as_str().unwrap().starts_with("agent error:"));
        assert!(r["agent_error"].as_str().unwrap().contains("PARITY-DEFERRED"));

        let notes = crate::db_json::query_rows(&c, "SELECT body_md FROM cockpit_blackboard WHERE pane_id = ? ORDER BY id", &[json!(pane_id)]).unwrap();
        let bodies: Vec<String> = notes.as_array().unwrap().iter().map(|r| r["body_md"].as_str().unwrap().to_string()).collect();
        assert!(bodies.iter().any(|b| b.starts_with("🧑 add a feature")));
        assert!(bodies.iter().any(|b| b.starts_with("🤖 agent error:")));

        let row = crate::db_json::query_rows(&c, "SELECT status FROM cockpit_panes WHERE id = ?", &[json!(pane_id)]).unwrap();
        assert_eq!(row[0]["status"], json!("error"));
    }

    #[test]
    fn user_text_alias_accepted_when_user_input_absent() {
        let c = seeded();
        let pane_id = insert_pane(&c, "dev", "", "[]", "agent");
        let r = cockpitPaneTurn(&json!({ "pane_id": pane_id, "user_text": "hello via alias" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false)); // still deferred, but the required-field guard passed
        let notes = crate::db_json::query_rows(&c, "SELECT body_md FROM cockpit_blackboard WHERE pane_id = ? ORDER BY id", &[json!(pane_id)]).unwrap();
        let bodies: Vec<String> = notes.as_array().unwrap().iter().map(|r| r["body_md"].as_str().unwrap().to_string()).collect();
        assert!(bodies.iter().any(|b| b.starts_with("🧑 hello via alias")));
    }

    #[test]
    fn mention_handoff_posts_note_to_sibling_pane() {
        let c = seeded();
        let pane_a = insert_pane(&c, "alice", "", "[]", "agent");
        let pane_b = insert_pane(&c, "bob", "", "[]", "agent");
        let r = cockpitPaneTurn(&json!({ "pane_id": pane_a, "user_input": "@bob please help" }), &c).unwrap();
        assert_eq!(r["pane_id"], json!(pane_a));
        let notes = crate::db_json::query_rows(&c, "SELECT body_md FROM cockpit_blackboard WHERE pane_id = ?", &[json!(pane_b)]).unwrap();
        let bodies: Vec<String> = notes.as_array().unwrap().iter().map(|r| r["body_md"].as_str().unwrap().to_string()).collect();
        assert!(bodies.iter().any(|b| b.contains("from alice")));
    }

    // ── buildPaneOverlay: persona / scope / mission / coordination / pane_files assembly ──────
    #[test]
    fn overlay_falls_back_persona_when_no_base_agent() {
        let c = seeded();
        let pane = json!({ "id": 7, "session_id": 1, "base_agent": Value::Null, "extra_prompt_md": Value::Null, "scope_json": "[]", "worktree_path": Value::Null });
        let overlay = build_pane_overlay(&pane, &c);
        assert_eq!(overlay["persona"], json!("(a focused implementation agent working within its scope)"));
        assert_eq!(overlay["mission_md"], json!("(none)"));
        assert_eq!(overlay["coordination"], json!("(none)"));
        assert_eq!(overlay["pane_files"], json!("(worktree not provisioned)"));
        assert_eq!(overlay["scope"], json!([]));
        assert_eq!(overlay["pane_id"], json!(7));
    }

    #[test]
    fn overlay_appends_extra_prompt_and_reads_mission_and_coordination() {
        let c = seeded();
        c.execute("UPDATE cockpit_sessions SET mission_md = 'the mission' WHERE id = 1", []).unwrap();
        insert_blackboard(&c, &json!(1), &Value::Null, "claim", "pane 0 claims src/**");
        let pane = json!({ "id": 3, "session_id": 1, "base_agent": Value::Null, "extra_prompt_md": "  do it carefully  ", "scope_json": "[\"src/**\"]", "worktree_path": Value::Null });
        let overlay = build_pane_overlay(&pane, &c);
        // Verified against real JS (skills/cockpit/cockpit-pane-turn.mjs buildPaneOverlay,
        // lines 32-35): persona starts '', base_agent is null so no resolvePersona call, then
        // extra_prompt_md is appended to the still-empty persona (no separator prefix since
        // persona is falsy at that point), and the "(a focused implementation agent...)"
        // fallback only fires when persona is STILL empty after the extra-prompt append — which
        // it isn't here. So the fallback text never appears in this case.
        assert_eq!(overlay["persona"], json!("OPERATOR EXTRA GUIDANCE:\ndo it carefully"));
        assert_eq!(overlay["mission_md"], json!("the mission"));
        assert!(overlay["coordination"].as_str().unwrap().contains("[claim] pane 0 claims src/**"));
        assert_eq!(overlay["scope"], json!(["src/**"]));
    }

    #[test]
    fn overlay_gathers_in_scope_files_when_worktree_provisioned() {
        let c = seeded();
        let dir = std::env::temp_dir().join(format!("antiloki-cockpit-pane-turn-rs-overlay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("keep.js"), "const x = 1;").unwrap();
        let pane = json!({ "id": 9, "session_id": 1, "base_agent": Value::Null, "extra_prompt_md": Value::Null, "scope_json": "[\"*.js\"]", "worktree_path": dir.to_string_lossy() });
        let overlay = build_pane_overlay(&pane, &c);
        assert!(overlay["pane_files"].as_str().unwrap().contains("--- keep.js ---"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
