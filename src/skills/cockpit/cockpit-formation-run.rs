//! cockpit-formation-run.rs — twin of skills/cockpit/cockpit-formation-run.mjs. NAMES ARE
//! CONTRACT: cockpitFormationRun (default). EPIC-VIBE-COCKPIT full-n8n slice — the FORMATION
//! RUNNER: walks a collection's wired panes (nodes) in step order. An AGENT step FIRES one
//! cockpit-pane-message turn and EMITs its reply as a tagged `[step:<label>]` shared doc that
//! downstream steps' `reads` pick up; a TOOL step (shape/judgment/kanban/…) cannot fire
//! autonomously — the run PAUSES there until the operator acts + `done-manual` advances. Run
//! state is ONE session-level blackboard row (kind 'flowrun') per collection, update-in-place.
//!
//! BESPOKE-NATIVE, not conform-db: the 'next' op on an agent step dispatches to the
//! already-converted `cockpit_cockpit_pane_message::cockpitPaneMessage` twin, which is itself
//! bespoke-native (real fs + a PARITY-DEFERRED `aiCall` — see that module's doc). This twin
//! reuses it verbatim rather than re-deriving the agent-turn machinery; everything else
//! (discoverSteps' step-wiring defaults, the blackboard-backed run state machine, gatherInputs'
//! channel resolution via the already-converted `cockpit__helpers_context_brief::resolveSourceText`,
//! frameTitle's board_json parse) is ported faithfully and deterministically.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } }
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }
fn num_param(n: f64) -> Value { if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) } }
fn js_num_eq(v: Option<&Value>, n: f64) -> bool { matches!(v, Some(Value::Number(x)) if x.as_f64() == Some(n)) }

const AGENT_KIND: &str = "agent"; // = AGENT_KINDS Set — only plain agent panes fire autonomously

fn read_run(db: &Connection, session_id: &Value, col_n: f64) -> Option<(i64, Value)> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, body_md FROM cockpit_blackboard WHERE session_id = ? AND pane_id IS NULL AND kind = 'flowrun' ORDER BY id DESC",
        &[session_id.clone()],
    ).ok()?;
    for r in rows.as_array()?.iter() {
        let body = r.get("body_md").and_then(Value::as_str).unwrap_or("");
        if let Ok(v) = serde_json::from_str::<Value>(body) {
            if js_num_eq(v.get("col"), col_n) {
                let id = r.get("id").and_then(Value::as_i64).unwrap_or(0);
                return Some((id, v));
            }
        }
    }
    None
}

fn save_run(db: &Connection, session_id: &Value, existing_id: Option<i64>, run: &Value) -> Result<(), String> {
    let body = take_chars(&serde_json::to_string(run).unwrap_or_default(), 19000);
    match existing_id {
        Some(id) => { crate::db_json::run_write(db, "UPDATE cockpit_blackboard SET body_md = ? WHERE id = ?", &[json!(body), json!(id)])?; }
        None => { crate::db_json::run_write(db, "INSERT INTO cockpit_blackboard (session_id, pane_id, kind, body_md) VALUES (?, NULL, 'flowrun', ?)", &[session_id.clone(), json!(body)])?; }
    }
    Ok(())
}

struct StepRec {
    pane_id: Value,
    idx: i64,
    label: String,
    kind: String,
    role: String,
    order: f64,
    reads: Option<Vec<String>>,
    emit: Option<String>,
}

// the formation's nodes, in step order, from the panes' own wiring
fn discover_steps(db: &Connection, session_id: &Value, col_n: f64) -> Vec<Value> {
    let panes = crate::db_json::query_rows(
        db,
        "SELECT id, idx, subagent_name, pane_kind, settings_json FROM cockpit_panes WHERE session_id = ? ORDER BY idx",
        &[session_id.clone()],
    ).unwrap_or(json!([]));
    let mut recs: Vec<StepRec> = Vec::new();
    if let Some(arr) = panes.as_array() {
        for p in arr {
            let st_raw = p.get("settings_json").and_then(Value::as_str).unwrap_or("{}");
            let st: Value = serde_json::from_str(st_raw).unwrap_or(json!({}));
            let st = if st.is_object() { st } else { json!({}) };
            if !js_num_eq(st.get("collection"), col_n) { continue; }
            let pane_kind_s = str_qq(p.get("pane_kind"));
            let role_upper = str_qq(st.get("role")).to_uppercase();
            if pane_kind_s == "orchestrator" || role_upper == "ORCH" { continue; } // the conductor, not a step
            let idx = p.get("idx").and_then(Value::as_i64).unwrap_or(0);
            let label = match nz(p.get("subagent_name")) {
                Some(v) => str_qq(Some(v)),
                None => format!("pane {}", idx + 1),
            };
            let kind = match nz(p.get("pane_kind")) {
                Some(v) => str_qq(Some(v)),
                None => "agent".to_string(),
            };
            let role = match nz(st.get("role")).or_else(|| nz(p.get("pane_kind"))) {
                Some(v) => str_qq(Some(v)),
                None => String::new(),
            };
            let order = match st.get("step") {
                Some(Value::Number(n)) if n.as_f64().map_or(false, f64::is_finite) => n.as_f64().unwrap(),
                _ => 1000.0 + idx as f64, // wired order wins; board order backs it
            };
            let reads = match st.get("reads") {
                Some(Value::Array(a)) => Some(a.iter().map(js_string).collect::<Vec<_>>()),
                _ => None, // null = the default chain
            };
            let emit_raw = str_qq(nz(st.get("emit")));
            let emit = if emit_raw.is_empty() { None } else { Some(emit_raw) };
            recs.push(StepRec { pane_id: p.get("id").cloned().unwrap_or(Value::Null), idx, label, kind, role, order, reads, emit });
        }
    }
    recs.sort_by(|a, b| a.order.partial_cmp(&b.order).unwrap_or(std::cmp::Ordering::Equal));

    // defaults that make ANY formation runnable: read mission+shared+the previous
    // step's output; emit under your own label
    let mut out: Vec<Value> = Vec::with_capacity(recs.len());
    let mut emits: Vec<String> = Vec::with_capacity(recs.len());
    for (i, r) in recs.iter().enumerate() {
        let emit = r.emit.clone().unwrap_or_else(|| format!("step:{}", r.label));
        emits.push(emit.clone());
        let reads = r.reads.clone().unwrap_or_else(|| {
            let mut v = vec!["mission".to_string(), "shared".to_string()];
            if i > 0 { v.push(emits[i - 1].clone()); }
            v
        });
        out.push(json!({
            "pane_id": r.pane_id,
            "idx": r.idx,
            "label": r.label,
            "kind": r.kind,
            "role": r.role,
            "order": num_param(r.order),
            "reads": reads,
            "emit": emit,
            "status": "pending",
        }));
    }
    out
}

fn frame_title(db: &Connection, session_id: &Value, col_n: f64) -> Value {
    (|| -> Option<Value> {
        let rows = crate::db_json::query_rows(db, "SELECT board_json FROM cockpit_sessions WHERE id = ?", &[session_id.clone()]).ok()?;
        let bj = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("board_json")).and_then(Value::as_str).map(|s| s.to_string());
        let src1 = bj.clone().unwrap_or_else(|| "{}".to_string());
        let parsed1: Value = serde_json::from_str(&src1).ok()?;
        let frames = match parsed1.get("frames") {
            Some(v) if !v.is_null() => v.clone(),
            _ => {
                let src2 = bj.unwrap_or_else(|| "[]".to_string());
                serde_json::from_str::<Value>(&src2).ok()?
            }
        };
        let arr = frames.as_array()?;
        let found = arr.iter().find(|f| js_num_eq(f.get("id"), col_n))?;
        match found.get("title") {
            Some(t) if !t.is_null() => Some(t.clone()),
            _ => None,
        }
    })().unwrap_or(Value::Null)
}

// gather ONE step's inputs from its declared channels
fn gather_inputs(db: &Connection, session_id: &Value, reads: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let per_ch_cap = ((7000f64 / reads.len().max(1) as f64).floor() as usize).max(700);
    for ch in reads.iter().take(8) {
        let mut text = String::new();
        if ch == "mission" {
            if let Ok(rows) = crate::db_json::query_rows(db, "SELECT mission_md FROM cockpit_sessions WHERE id = ?", &[session_id.clone()]) {
                text = str_qq(rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("mission_md")));
            }
        } else if ch == "shared" {
            if let Ok(rows) = crate::db_json::query_rows(
                db,
                "SELECT body_md FROM cockpit_blackboard WHERE session_id = ? AND kind = 'context' AND body_md NOT LIKE '[step:%' ORDER BY id DESC LIMIT 6",
                &[session_id.clone()],
            ) {
                text = rows.as_array().map(|a| a.iter().map(|r| str_qq(r.get("body_md"))).collect::<Vec<_>>().join("\n---\n")).unwrap_or_default();
            }
        } else if ch == "digest" {
            text = str_qq(Some(&crate::cockpit__helpers_context_brief::resolveSourceText(db, &json!("digest"), &json!({}))));
        } else if ch.starts_with("step:") {
            if let Ok(rows) = crate::db_json::query_rows(
                db,
                "SELECT body_md FROM cockpit_blackboard WHERE session_id = ? AND kind = 'context' AND body_md LIKE ? ORDER BY id DESC LIMIT 2",
                &[session_id.clone(), json!(format!("[{ch}]%"))],
            ) {
                text = rows.as_array().map(|a| a.iter().map(|r| str_qq(r.get("body_md"))).collect::<Vec<_>>().join("\n---\n")).unwrap_or_default();
            }
        } else {
            text = str_qq(Some(&crate::cockpit__helpers_context_brief::resolveSourceText(db, &json!(ch.clone()), &json!({}))));
        }
        if !text.trim().is_empty() {
            parts.push(format!("【{ch}】\n{}", take_chars(&text, per_ch_cap)));
        }
    }
    let joined = take_chars(&parts.join("\n\n"), 8000);
    if joined.is_empty() { "(no inputs resolved — deliver from the step brief alone)".to_string() } else { joined }
}

fn push_log(run: &mut Value, entry: Value) {
    let mut log = run.get("log").and_then(Value::as_array).cloned().unwrap_or_default();
    log.push(entry);
    let len = log.len();
    let trimmed = if len > 12 { log[len - 12..].to_vec() } else { log };
    run["log"] = json!(trimmed);
}

pub fn cockpitFormationRun(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));
    let session_id_n = js_number(input.get("session_id"));
    let col_n = js_number(input.get("col"));
    let op = input.get("op").and_then(Value::as_str).unwrap_or("status").to_string();
    if !(session_id_n.is_finite() && session_id_n != 0.0) {
        return Err("cockpit-formation-run: session_id is required".into());
    }
    if !(col_n.is_finite() && col_n != 0.0) {
        return Err("cockpit-formation-run: col is required".into());
    }
    let session_id = num_param(session_id_n);
    let col = num_param(col_n);

    let existing = read_run(db, &session_id, col_n);

    if op == "status" {
        return Ok(json!({ "ok": true, "run": existing.as_ref().map(|(_, r)| r.clone()).unwrap_or(Value::Null) }));
    }

    if op == "start" || op == "reset" {
        let steps = discover_steps(db, &session_id, col_n);
        if steps.is_empty() {
            return Ok(json!({ "ok": false, "error": "this collection has no step panes — spawn a formation (or wire steps) first" }));
        }
        let mut run = Map::new();
        run.insert("col".into(), col.clone());
        run.insert("title".into(), frame_title(db, &session_id, col_n));
        run.insert("status".into(), json!(if op == "reset" { "idle" } else { "running" }));
        run.insert("current".into(), json!(0));
        run.insert("steps".into(), json!(steps));
        run.insert("log".into(), json!([]));
        let run_val = Value::Object(run);
        save_run(db, &session_id, existing.as_ref().map(|(id, _)| *id), &run_val)?;
        return Ok(json!({ "ok": true, "run": run_val }));
    }

    let (existing_id, mut run) = match existing {
        Some((id, r)) => (id, r),
        None => return Ok(json!({ "ok": false, "error": "no run yet — start the formation first" })),
    };

    if op == "pause" {
        run["status"] = json!("paused");
        save_run(db, &session_id, Some(existing_id), &run)?;
        return Ok(json!({ "ok": true, "run": run }));
    }
    if op == "resume" {
        let st = run.get("status").and_then(Value::as_str).unwrap_or("").to_string();
        if st == "paused" || st == "paused_tool" || st == "error" { run["status"] = json!("running"); }
        save_run(db, &session_id, Some(existing_id), &run)?;
        return Ok(json!({ "ok": true, "run": run }));
    }

    if op == "done-manual" {
        let current = run.get("current").and_then(Value::as_i64).unwrap_or(0);
        let steps_len = run.get("steps").and_then(Value::as_array).map(|a| a.len() as i64).unwrap_or(0);
        if current < 0 || current >= steps_len {
            return Ok(json!({ "ok": false, "error": "no current step" }));
        }
        let idx = current as usize;
        let label = run["steps"][idx].get("label").cloned().unwrap_or(Value::Null);
        run["steps"][idx]["status"] = json!("done");
        run["steps"][idx]["manual"] = json!(true);
        push_log(&mut run, json!({ "step": label, "how": "manual", "at": crate::clock::now_iso_nodb() }));
        let new_current = current + 1;
        run["current"] = json!(new_current);
        run["status"] = json!(if new_current >= steps_len { "done" } else { "running" });
        save_run(db, &session_id, Some(existing_id), &run)?;
        return Ok(json!({ "ok": true, "run": run }));
    }

    if op == "next" {
        let status = run.get("status").and_then(Value::as_str).unwrap_or("").to_string();
        if status == "done" {
            return Ok(json!({ "ok": true, "run": run }));
        }
        if status != "running" && status != "paused_tool" {
            return Ok(json!({ "ok": false, "error": format!("run is {status} — resume first"), "run": run }));
        }
        let current = run.get("current").and_then(Value::as_i64).unwrap_or(0);
        let steps_len = run.get("steps").and_then(Value::as_array).map(|a| a.len() as i64).unwrap_or(0);
        if current < 0 || current >= steps_len {
            run["status"] = json!("done");
            save_run(db, &session_id, Some(existing_id), &run)?;
            return Ok(json!({ "ok": true, "run": run }));
        }
        let idx = current as usize;
        let s_kind = str_qq(run["steps"][idx].get("kind"));
        // a TOOL node cannot fire autonomously — pause; the operator acts + done-manual
        if s_kind != AGENT_KIND {
            run["status"] = json!("paused_tool");
            run["steps"][idx]["status"] = json!("waiting_operator");
            save_run(db, &session_id, Some(existing_id), &run)?;
            let label = run["steps"][idx].get("label").cloned().unwrap_or(Value::Null);
            return Ok(json!({ "ok": true, "paused": "tool", "step": label, "kind": s_kind, "run": run }));
        }
        run["steps"][idx]["status"] = json!("running");
        run["status"] = json!("running");
        save_run(db, &session_id, Some(existing_id), &run)?;

        let step_val = run["steps"][idx].clone();
        let reads: Vec<String> = step_val.get("reads").and_then(Value::as_array).map(|a| a.iter().map(|v| str_qq(Some(v))).collect()).unwrap_or_default();
        let inputs_text = gather_inputs(db, &session_id, &reads);
        let label = str_qq(step_val.get("label"));
        let role = str_qq(step_val.get("role"));
        let title = str_qq(run.get("title"));
        let title_display = if title.is_empty() { "the collection".to_string() } else { title };
        let role_suffix = if !role.is_empty() && role != label { format!(" ({role})") } else { String::new() };
        let emit = str_qq(step_val.get("emit"));
        let brief = format!(
            "You are STEP {}/{} — \"{label}\"{role_suffix} of the formation \"{title_display}\".\n\nYOUR INPUTS (the channels wired into this step):\n{inputs_text}\n\nDeliver THIS STEP'S OUTPUT now — one focused answer. It is handed to the next steps as {emit}.",
            current + 1,
            steps_len,
        );

        let pane_id = step_val.get("pane_id").cloned().unwrap_or(Value::Null);
        match crate::cockpit_cockpit_pane_message::cockpitPaneMessage(&json!({ "pane_id": pane_id, "user_input": brief }), db) {
            Ok(res) => {
                let reply = str_qq(res.get("reply_md")).trim().to_string();
                // EMIT — the step's output becomes a tagged shared doc downstream reads pick up
                let body = take_chars(&format!("[{emit}] {}", if reply.is_empty() { "(the step returned nothing)".to_string() } else { reply.clone() }), 19000);
                crate::db_json::run_write(db, "INSERT INTO cockpit_blackboard (session_id, pane_id, kind, body_md) VALUES (?, NULL, 'context', ?)", &[session_id.clone(), json!(body)])?;
                run["steps"][idx]["status"] = json!("done");
                let ai_call_id = res.get("ai_call_id").cloned().unwrap_or(Value::Null);
                push_log(&mut run, json!({ "step": label, "how": "fired", "ai_call_id": ai_call_id, "at": crate::clock::now_iso_nodb() }));
                let new_current = current + 1;
                run["current"] = json!(new_current);
                run["status"] = json!(if new_current >= steps_len { "done" } else { "running" });
                save_run(db, &session_id, Some(existing_id), &run)?;
                Ok(json!({ "ok": true, "fired": label, "reply_md": take_chars(&reply, 4000), "ai_call_id": ai_call_id, "run": run }))
            }
            Err(e) => {
                run["steps"][idx]["status"] = json!("error");
                run["status"] = json!("error");
                push_log(&mut run, json!({ "step": label, "how": "error", "error": take_chars(&e, 200), "at": crate::clock::now_iso_nodb() }));
                save_run(db, &session_id, Some(existing_id), &run)?;
                Ok(json!({ "ok": false, "error": format!("step \"{label}\" failed: {e}"), "run": run }))
            }
        }
    } else {
        Err(format!(
            "cockpit-formation-run: op must be one of \"start\", \"next\", \"done-manual\", \"pause\", \"resume\", \"reset\", \"status\" — got \"{op}\""
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_env<T>(k: &str, v: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        match v {
            Some(val) => unsafe { std::env::set_var(k, val) },
            None => unsafe { std::env::remove_var(k) },
        }
        let result = f();
        unsafe { std::env::remove_var(k) };
        result
    }

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) ); \
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.ai_calls ( id INTEGER PRIMARY KEY, cost_usd_actual REAL ); \
             CREATE TABLE cost_settings (key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
        c
    }

    fn insert_session(c: &Connection, board_json: &str) {
        crate::db_json::run_write(c, "INSERT INTO cockpit_sessions (id, name, mission_md, board_json) VALUES (1, 'sess', 'ship it', ?)", &[json!(board_json)]).unwrap();
    }

    fn insert_pane(c: &Connection, idx: i64, subagent_name: &str, pane_kind: &str, settings_json: &str) -> i64 {
        crate::db_json::run_write(
            c,
            "INSERT INTO cockpit_panes (session_id, idx, subagent_name, pane_kind, settings_json) VALUES (1, ?, ?, ?, ?)",
            &[json!(idx), json!(subagent_name), json!(pane_kind), json!(settings_json)],
        ).unwrap().1
    }

    #[test]
    fn status_with_no_run_yields_null() {
        let c = seeded();
        insert_session(&c, "[]");
        let r = cockpitFormationRun(&json!({ "session_id": 1, "col": 77 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "run": null }));
    }

    #[test]
    fn start_with_no_step_panes_errors() {
        let c = seeded();
        insert_session(&c, "[]");
        let r = cockpitFormationRun(&json!({ "session_id": 1, "col": 77, "op": "start" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("no step panes"));
    }

    #[test]
    fn start_discovers_steps_orch_excluded_defaults_wired() {
        let c = seeded();
        insert_session(&c, &json!([{ "id": 77, "title": "my flow" }]).to_string());
        insert_pane(&c, 0, "conductor", "orchestrator", &json!({ "collection": 77, "role": "ORCH" }).to_string());
        insert_pane(&c, 1, "alice", "agent", &json!({ "collection": 77, "step": 1 }).to_string());
        insert_pane(&c, 2, "shaper", "shape", &json!({ "collection": 77, "step": 2 }).to_string());
        insert_pane(&c, 3, "other-col", "agent", &json!({ "collection": 5 }).to_string());
        let r = cockpitFormationRun(&json!({ "session_id": 1, "col": 77, "op": "start" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let run = &r["run"];
        assert_eq!(run["title"], json!("my flow"));
        assert_eq!(run["status"], json!("running"));
        let steps = run["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["label"], json!("alice"));
        assert_eq!(steps[0]["emit"], json!("step:alice"));
        assert_eq!(steps[0]["reads"], json!(["mission", "shared"]));
        assert_eq!(steps[1]["label"], json!("shaper"));
        assert_eq!(steps[1]["kind"], json!("shape"));
        assert_eq!(steps[1]["reads"], json!(["mission", "shared", "step:alice"]));
    }

    #[test]
    fn next_on_tool_step_pauses_and_done_manual_advances() {
        let c = seeded();
        insert_session(&c, "[]");
        insert_pane(&c, 0, "shaper", "shape", &json!({ "collection": 1, "step": 1 }).to_string());
        cockpitFormationRun(&json!({ "session_id": 1, "col": 1, "op": "start" }), &c).unwrap();
        let r = cockpitFormationRun(&json!({ "session_id": 1, "col": 1, "op": "next" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["paused"], json!("tool"));
        assert_eq!(r["run"]["status"], json!("paused_tool"));

        let r2 = cockpitFormationRun(&json!({ "session_id": 1, "col": 1, "op": "done-manual" }), &c).unwrap();
        assert_eq!(r2["ok"], json!(true));
        assert_eq!(r2["run"]["status"], json!("done"));
        assert_eq!(r2["run"]["current"], json!(1));
        assert_eq!(r2["run"]["steps"][0]["status"], json!("done"));
    }

    #[test]
    fn next_on_agent_step_fires_pane_message_and_emits() {
        with_env("ANTILOKI_VIBE_CHAT", Some("off"), || {
            let c = seeded();
            insert_session(&c, "[]");
            let pane_id = insert_pane(&c, 0, "dev", "agent", &json!({ "collection": 1, "step": 1 }).to_string());
            crate::db_json::run_write(&c, "UPDATE cockpit_panes SET worktree_path = '' WHERE id = ?", &[json!(pane_id)]).unwrap();
            cockpitFormationRun(&json!({ "session_id": 1, "col": 1, "op": "start" }), &c).unwrap();
            let r = cockpitFormationRun(&json!({ "session_id": 1, "col": 1, "op": "next" }), &c).unwrap();
            assert_eq!(r["ok"], json!(true));
            assert_eq!(r["fired"], json!("dev"));
            assert_eq!(r["run"]["status"], json!("done"));
            assert_eq!(r["run"]["current"], json!(1));
            let docs = crate::db_json::query_rows(&c, "SELECT body_md FROM cockpit_blackboard WHERE kind = 'context'", &[]).unwrap();
            let bodies: Vec<String> = docs.as_array().unwrap().iter().map(|d| d["body_md"].as_str().unwrap().to_string()).collect();
            assert!(bodies.iter().any(|b| b.starts_with("[step:dev]")));
        });
    }

    #[test]
    fn pause_and_resume_toggle_status() {
        let c = seeded();
        insert_session(&c, "[]");
        insert_pane(&c, 0, "shaper", "shape", &json!({ "collection": 1, "step": 1 }).to_string());
        cockpitFormationRun(&json!({ "session_id": 1, "col": 1, "op": "start" }), &c).unwrap();
        let r = cockpitFormationRun(&json!({ "session_id": 1, "col": 1, "op": "pause" }), &c).unwrap();
        assert_eq!(r["run"]["status"], json!("paused"));
        let r2 = cockpitFormationRun(&json!({ "session_id": 1, "col": 1, "op": "resume" }), &c).unwrap();
        assert_eq!(r2["run"]["status"], json!("running"));
    }

    #[test]
    fn missing_session_id_or_col_errors() {
        let c = seeded();
        assert!(cockpitFormationRun(&json!({ "col": 1 }), &c).is_err());
        assert!(cockpitFormationRun(&json!({ "session_id": 1 }), &c).is_err());
    }

    #[test]
    fn next_before_start_errors() {
        let c = seeded();
        insert_session(&c, "[]");
        let r = cockpitFormationRun(&json!({ "session_id": 1, "col": 1, "op": "next" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "no run yet — start the formation first" }));
    }
}
