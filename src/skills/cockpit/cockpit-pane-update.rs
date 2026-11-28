//! cockpit-pane-update.rs — twin of skills/cockpit/cockpit-pane-update.mjs (db WRITE lane). NAMES ARE
//! CONTRACT: `cockpitPaneUpdate` (default). Two jobs: EDIT a pane's config (partial — only provided
//! fields change) or REORDER a session's panes (idx offset-by-1000 dodge then reassign 0..n-1).
// QUALITY: 78/100 — sparse SET-clause builder over ~15 optional fields; reorder branch is a 2-pass UPDATE
use rusqlite::Connection;
use serde_json::{json, Map, Value};

const TIERS: &[&str] = &["low", "mid", "high"];
/// the states a pane may be put in through the action — an allowlist, so a typo cannot park it somewhere unread
const PANE_STATUSES: &[&str] = &["idle", "ready", "running", "closed", "failed", "paused"];
// THE FIVE SEATS ARE ALL PANE KINDS (2026-09-28). `gemini`, `grok` and `cursor` were missing, and an unknown
// kind is silently rewritten to "agent" — so a fork of a Cursor agent, or a switch onto one, produced a pane whose
// kind was the word "agent", which is not an engine: unstartable, and unswitchable because the switch has to start
// it. Found on pane 50. Keep this list and `serve::engines::ENGINES` in step.
const PANE_KINDS: &[&str] = &[
    "agent", "opencode", "opencode-direct", "claude-code", "codex", "gemini", "grok", "cursor", "script", "git", "context", "notes",
    "state", "docs", "run", "gate", "code", "diagram", "graph", "kanban", "standup", "shell", "review",
    "todo", "race", "timeline", "design", "sketch", "fuel", "notebook", "radar", "ultraplan", "breakdown",
    "judgment", "swarm", "intent", "orchestrator",
];
const SCRIPT_KINDS: &[&str] = &["build", "dev", "test"];

fn role_grants(base_agent: &str) -> Option<Value> {
    match base_agent {
        "dev" => Some(json!(["read", "edit"])),
        "qa" => Some(json!(["read", "run"])),
        "pl" => Some(json!(["read"])),
        "tl" => Some(json!(["read"])),
        "man" => Some(json!(["read"])),
        _ => None,
    }
}

// JS `String(v ?? '')`.
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS `String(v)` (no ?? — null/undefined render as the literal words).
fn js_string(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(Value::Array(a)) => a.iter().map(|x| js_string(Some(x))).collect::<Vec<_>>().join(","),
        Some(Value::Object(_)) => "[object Object]".into(),
    }
}
// JS `Number(v)`.
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn truthy_num(n: f64) -> bool { n != 0.0 && !n.is_nan() }
// node:sqlite binds an integer-valued JS number as INTEGER, else REAL — mirror the bind type.
fn num_param(n: f64) -> Value { if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) } }
// JS String.prototype.slice(0, n) over BMP-safe fixtures.
fn take(s: String, n: usize) -> String { s.chars().take(n).collect() }
// = JS `${v}` for a whole-valued Number.
fn num_tmpl(n: f64) -> String { if n.is_finite() && n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 { format!("{}", n as i64) } else { n.to_string() } }

// JS: try { JSON.parse(s ?? 'null') ?? d } catch { d }
fn parse_or(s: Option<&Value>, default: Value) -> Value {
    let text = match s { Some(Value::String(t)) => t.clone(), None | Some(Value::Null) => "null".to_string(), Some(o) => o.to_string() };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Null) => default,
        Ok(v) => v,
        Err(_) => default,
    }
}

pub fn cockpitPaneUpdate(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    // REORDER branch
    if let Some(order) = input.get("order").and_then(|v| v.as_array()) {
        let session_id = js_number(input.get("session_id"));
        if !truthy_num(session_id) { return Err("cockpit-pane-update: session_id is required".into()); }
        let sid = num_param(session_id);
        crate::db_json::run_write(db, "UPDATE cockpit_panes SET idx = idx + 1000 WHERE session_id = ?", &[sid.clone()])?;
        for (i, pid) in order.iter().enumerate() {
            let pane_num = num_param(js_number(Some(pid)));
            crate::db_json::run_write(
                db,
                "UPDATE cockpit_panes SET idx = ? WHERE id = ? AND session_id = ?",
                &[json!(i as i64), pane_num, sid.clone()],
            )?;
        }
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("reordered".into(), json!(order.len() as i64));
        return Ok(Value::Object(out));
    }

    // EDIT branch
    let pane_id = js_number(input.get("pane_id"));
    if !truthy_num(pane_id) { return Err("cockpit-pane-update: pane_id is required".into()); }
    let pid_param = num_param(pane_id);

    let pane_rows = crate::db_json::query_rows(db, "SELECT * FROM cockpit_panes WHERE id = ?", &[pid_param.clone()])?;
    let prev_row: Value = pane_rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
    if pane_rows.as_array().map_or(true, |a| a.is_empty()) {
        let mut out = Map::new();
        out.insert("ok".into(), json!(false));
        out.insert("error".into(), json!(format!("pane {} not found", num_tmpl(pane_id))));
        return Ok(Value::Object(out));
    }

    let mut sets: Vec<&str> = Vec::new();
    let mut vals: Vec<Value> = Vec::new();
    let mut overlaps: Vec<Value> = Vec::new();

    let has_base_agent = input.get("base_agent").is_some();
    if has_base_agent {
        let s = take(js_string(input.get("base_agent")), 80);
        sets.push("base_agent = ?");
        vals.push(if s.is_empty() { Value::Null } else { json!(s) });
    }
    if input.get("subagent_name").is_some() {
        sets.push("subagent_name = ?");
        vals.push(json!(take(js_string(input.get("subagent_name")), 80)));
    }
    let has_grants = input.get("grants").is_some();
    if input.get("scope").is_some() {
        let arr: Vec<Value> = match input.get("scope").and_then(|v| v.as_array()) {
            Some(a) => a.iter().map(|x| json!(js_string(Some(x)))).collect(),
            None => Vec::new(),
        };
        // CLAIMS FOLLOW THE SCOPE (2026-09-16), and an overlap with another pane's claim is reported,
        // not refused (operator 2026-09-20: every pane has its own worktree — overlap matters at the merge)
        let session_id = prev_row.get("session_id").cloned().unwrap_or(Value::Null);
        let others = crate::db_json::query_rows(db, "SELECT pane_id, path_glob FROM cockpit_claims WHERE session_id = ? AND pane_id <> ?", &[session_id.clone(), pid_param.clone()])?;
        for g in &arr {
            for c in others.as_array().into_iter().flatten().filter(|c| crate::cockpit__helpers_scope::globsOverlap(g, c.get("path_glob").unwrap_or(&Value::Null)).as_bool().unwrap_or(false)) {
                overlaps.push(json!({ "scope": g, "pane_id": c.get("pane_id").cloned().unwrap_or(Value::Null), "glob": c.get("path_glob").cloned().unwrap_or(Value::Null) }));
            }
        }
        crate::db_json::run_write(db, "DELETE FROM cockpit_claims WHERE pane_id = ?", &[pid_param.clone()])?;
        for g in &arr {
            crate::db_json::run_write(db, "INSERT INTO cockpit_claims (session_id, pane_id, path_glob) VALUES (?,?,?)", &[session_id.clone(), pid_param.clone(), g.clone()])?;
        }
        sets.push("scope_json = ?");
        vals.push(json!(serde_json::to_string(&arr).unwrap_or_else(|_| "[]".into())));
    }
    if has_grants {
        let arr: Vec<Value> = match input.get("grants").and_then(|v| v.as_array()) {
            Some(a) => a.iter().map(|x| json!(js_string(Some(x)))).collect(),
            None => Vec::new(),
        };
        sets.push("grants_json = ?");
        vals.push(json!(serde_json::to_string(&arr).unwrap_or_else(|_| "[]".into())));
    } else if has_base_agent {
        if let Some(g) = role_grants(&js_string(input.get("base_agent"))) {
            sets.push("grants_json = ?");
            vals.push(json!(serde_json::to_string(&g).unwrap_or_else(|_| "[]".into())));
        }
    }
    if input.get("tier").is_some() {
        let t = input.get("tier").and_then(|v| v.as_str());
        let picked = match t { Some(x) if TIERS.contains(&x) => x, _ => "low" };
        sets.push("tier = ?");
        vals.push(json!(picked));
    }
    if input.get("extra_prompt_md").is_some() {
        let v = input.get("extra_prompt_md");
        sets.push("extra_prompt_md = ?");
        vals.push(if truthy(v) { json!(take(js_string(v), 8000)) } else { Value::Null });
    }
    if input.get("model_id").is_some() {
        let v = input.get("model_id");
        sets.push("model_id = ?");
        vals.push(if truthy(v) { json!(take(js_string(v), 200)) } else { Value::Null });
    }
    // THE TWO LIFECYCLE COLUMNS, SETTABLE THROUGH THE ACTION AT LAST (audit 2026-09-28). `worktree_path` and
    // `status` had no lane here, which is why three places in serve wrote them with raw SQL against the substrate
    // — the deviation the architecture rule exists to prevent. `status` is an allowlist, not free text, so a typo
    // cannot quietly park a pane in a state nothing reads.
    if input.get("worktree_path").is_some() {
        let v = input.get("worktree_path");
        sets.push("worktree_path = ?");
        vals.push(if truthy(v) { json!(take(js_string(v), 4096)) } else { Value::Null });
    }
    if input.get("status").is_some() {
        let s = js_string(input.get("status"));
        if PANE_STATUSES.contains(&s.as_str()) { sets.push("status = ?"); vals.push(json!(s)); }
    }
    if input.get("auto_apply").is_some() {
        sets.push("auto_apply = ?");
        vals.push(json!(if truthy(input.get("auto_apply")) { 1 } else { 0 }));
    }
    if input.get("pane_kind").is_some() {
        let s = js_string(input.get("pane_kind"));
        sets.push("pane_kind = ?");
        vals.push(json!(if PANE_KINDS.contains(&s.as_str()) { s } else { "agent".to_string() }));
    }
    if input.get("span_cols").is_some() {
        let n = js_number(input.get("span_cols"));
        let n = if !truthy_num(n) { 1.0 } else { n };
        let clamped = n.max(1.0).min(3.0);
        sets.push("span_cols = ?");
        vals.push(num_param(clamped));
    }
    if input.get("span_rows").is_some() {
        let n = js_number(input.get("span_rows"));
        let n = if !truthy_num(n) { 1.0 } else { n };
        let clamped = n.max(1.0).min(3.0);
        sets.push("span_rows = ?");
        vals.push(num_param(clamped));
    }
    if input.get("script_kind").is_some() {
        let s = js_string(input.get("script_kind"));
        sets.push("script_kind = ?");
        vals.push(if SCRIPT_KINDS.contains(&s.as_str()) { json!(s) } else { Value::Null });
    }
    if input.get("script_worktree_pane").is_some() {
        let n = js_number(input.get("script_worktree_pane"));
        sets.push("script_worktree_pane = ?");
        vals.push(if truthy_num(n) { num_param(n) } else { Value::Null });
    }
    if input.get("settings").is_some() {
        let v = input.get("settings");
        let is_obj = matches!(v, Some(Value::Object(_)) | Some(Value::Array(_)));
        sets.push("settings_json = ?");
        vals.push(if truthy(v) && is_obj {
            let s = serde_json::to_string(v.unwrap()).unwrap_or_else(|_| "null".into());
            json!(take(s, 12000))
        } else {
            Value::Null
        });
    }
    if input.get("context_excludes").is_some() {
        let arr: Vec<Value> = match input.get("context_excludes").and_then(|v| v.as_array()) {
            Some(a) => a
                .iter()
                .map(|x| js_number(Some(x)))
                .filter(|n| truthy_num(*n))
                .map(num_param)
                .collect(),
            None => Vec::new(),
        };
        sets.push("context_excludes_json = ?");
        vals.push(json!(serde_json::to_string(&arr).unwrap_or_else(|_| "[]".into())));
    }

    if !sets.is_empty() {
        // FROZEN-CLOCK MIRROR: 'now' pinned to the conform harness instant.
        let sql = format!("UPDATE cockpit_panes SET {}, last_activity = {} WHERE id = ?", sets.join(", "), crate::clock::now_sql());
        let mut params = vals.clone();
        params.push(pid_param.clone());
        crate::db_json::run_write(db, &sql, &params)?;
    }

    let rows = crate::db_json::query_rows(db, "SELECT * FROM cockpit_panes WHERE id = ?", &[pid_param.clone()])?;
    let row = rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);

    // CONTEXT PACK (2026-09-14): a scope / guidance / settings edit used to promise a re-bake "on
    // the next boot" that nothing performed. Re-bake now (hash-skips when the pack is unchanged;
    // forced when the selection itself changed) — the brief block follows the pane, not its birth.
    let mut context_pack = Value::Null;
    // settings_json also carries layout (x/y), chips and bookkeeping — only a changed `context_pack`
    // block (or an explicit skills/scope/guidance edit) is a reason to touch the box
    let settings_pack_changed = input.get("settings").is_some() && {
        let before = prev_row.get("settings_json").map(|v| crate::cockpit__helpers_context_pack::parse_pack(v)).unwrap_or(Value::Null);
        let after = crate::cockpit__helpers_context_pack::parse_pack(input.get("settings").unwrap_or(&Value::Null));
        before.get("preset") != after.get("preset") || before.get("items") != after.get("items") || before.get("live") != after.get("live") || before.get("budget_chars") != after.get("budget_chars")
    };
    // THE antiQA BUTTON (Heimdall menu, `settings.antiqa.enabled` / `.packs`): the second MCP server enters or leaves the pane's engine
    // configs. The pack itself is unchanged, so this rides the bake's hash-skip path, which re-writes the wiring and the brief.
    let settings_antiqa_changed = input.get("settings").is_some() && {
        let pick = |v: Option<&Value>| -> Value {
            let parsed: Value = match v { Some(Value::String(t)) => serde_json::from_str(t).unwrap_or(Value::Null), Some(o) => o.clone(), None => Value::Null };
            parsed.get("antiqa").cloned().unwrap_or(Value::Null)
        };
        pick(prev_row.get("settings_json")) != pick(input.get("settings"))
    };
    let touches_pack = ["scope", "extra_prompt_md", "context_excludes"].iter().any(|k| input.get(*k).is_some()) || settings_pack_changed || settings_antiqa_changed;
    let has_worktree = row.get("worktree_path").and_then(Value::as_str).map_or(false, |p| !p.is_empty());
    if touches_pack && has_worktree && !matches!(input.get("context_bake"), Some(Value::Bool(false))) {
        let force = settings_pack_changed || input.get("scope").is_some() || input.get("extra_prompt_md").is_some();
        context_pack = crate::cockpit_cockpit_pane_context_bake::cockpitPaneContextBake(&json!({ "pane_id": row.get("id").cloned().unwrap_or(Value::Null), "force": force }), db).unwrap_or_else(|e| json!({ "ok": false, "error": e }));
    }
    let rows = crate::db_json::query_rows(db, "SELECT * FROM cockpit_panes WHERE id = ?", &[pid_param])?;
    let row = rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
    let mut pane = match row { Value::Object(m) => m, _ => Map::new() };
    let scope = parse_or(pane.get("scope_json"), json!([]));
    let grants = parse_or(pane.get("grants_json"), json!([]));
    pane.insert("scope".into(), scope);
    pane.insert("grants".into(), grants);

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("pane".into(), Value::Object(pane));
    out.insert("overlaps".into(), json!(overlaps));
    out.insert("context_pack".into(), context_pack);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c.execute(
            "INSERT INTO cockpit_panes (id, session_id, idx, subagent_name, scope_json, grants_json, tier, base_agent) \
             VALUES (1, 5, 0, 'dev-1', '[]', '[]', 'low', 'dev')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO cockpit_panes (id, session_id, idx, subagent_name, scope_json, grants_json, tier) \
             VALUES (2, 5, 1, 'dev-2', '[]', '[]', 'low')",
            [],
        )
        .unwrap();
        c
    }

    #[test]
    fn missing_pane_id_errors() {
        let c = seeded();
        assert!(cockpitPaneUpdate(&json!({}), &c).unwrap_err().contains("pane_id is required"));
    }

    #[test]
    fn unknown_pane_not_found() {
        let c = seeded();
        let r = cockpitPaneUpdate(&json!({ "pane_id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "pane 999 not found" }));
    }

    #[test]
    fn edits_fields_and_rederives_grants_from_role() {
        let c = seeded();
        let r = cockpitPaneUpdate(&json!({
            "pane_id": 1,
            "base_agent": "qa",
            "scope": ["src/**"],
            "tier": "mid",
            "extra_prompt_md": "focus on tests",
            "model_id": "gpt-x",
            "auto_apply": true,
            "pane_kind": "script",
            "span_cols": 5,
            "script_kind": "test",
            "script_worktree_pane": 2,
            "settings": {"a": 1},
            "context_excludes": [3, 0, "5", "bogus"]
        }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let p = &r["pane"];
        assert_eq!(p["base_agent"], json!("qa"));
        assert_eq!(p["scope"], json!(["src/**"]));
        assert_eq!(p["grants"], json!(["read", "run"])); // rederived from ROLE_GRANTS['qa']
        assert_eq!(p["tier"], json!("mid"));
        assert_eq!(p["extra_prompt_md"], json!("focus on tests"));
        assert_eq!(p["model_id"], json!("gpt-x"));
        assert_eq!(p["auto_apply"], json!(1));
        assert_eq!(p["pane_kind"], json!("script"));
        assert_eq!(p["span_cols"], json!(3)); // clamped to max 3
        assert_eq!(p["script_kind"], json!("test"));
        assert_eq!(p["script_worktree_pane"], json!(2));
        assert_eq!(p["settings_json"], json!("{\"a\":1}"));
        assert_eq!(p["context_excludes_json"], json!("[3,5]")); // 0/NaN filtered by Boolean
        assert_eq!(p["last_activity"], json!("2026-01-01 00:00:00"));
    }

    #[test]
    fn invalid_enum_values_fall_back_to_defaults() {
        let c = seeded();
        let r = cockpitPaneUpdate(&json!({ "pane_id": 2, "tier": "bogus", "pane_kind": "bogus", "script_kind": "bogus" }), &c).unwrap();
        let p = &r["pane"];
        assert_eq!(p["tier"], json!("low"));
        assert_eq!(p["pane_kind"], json!("agent"));
        assert_eq!(p["script_kind"], Value::Null);
    }

    #[test]
    fn explicit_grants_wins_over_role_derivation() {
        let c = seeded();
        let r = cockpitPaneUpdate(&json!({ "pane_id": 1, "base_agent": "qa", "grants": ["read"] }), &c).unwrap();
        assert_eq!(r["pane"]["grants"], json!(["read"]));
    }

    #[test]
    fn no_fields_leaves_row_unchanged_and_still_returns_scope_grants() {
        let c = seeded();
        let r = cockpitPaneUpdate(&json!({ "pane_id": 1 }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["pane"]["subagent_name"], json!("dev-1"));
        assert_eq!(r["pane"]["scope"], json!([]));
        assert_eq!(r["pane"]["last_activity"], Value::Null);
    }

    #[test]
    fn reorders_panes_via_offset_dodge() {
        let c = seeded();
        let r = cockpitPaneUpdate(&json!({ "session_id": 5, "order": [2, 1] }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "reordered": 2 }));
        let rows = crate::db_json::query_rows(&c, "SELECT id, idx FROM cockpit_panes ORDER BY id", &[]).unwrap();
        assert_eq!(rows, json!([{ "id": 1, "idx": 1 }, { "id": 2, "idx": 0 }]));
    }

    #[test]
    fn reorder_missing_session_id_errors() {
        let c = seeded();
        assert!(cockpitPaneUpdate(&json!({ "order": [1, 2] }), &c).unwrap_err().contains("session_id is required"));
    }

    #[test]
    fn scope_edit_rewrites_claims_and_reports_overlap_with_another_pane() {
        let c = seeded();
        c.execute("INSERT INTO cockpit_claims (session_id, pane_id, path_glob) VALUES (5, 2, 'src/ui/**')", []).unwrap();
        // an overlap is reported, never refused (operator 2026-09-20: each pane has its own worktree)
        let r = cockpitPaneUpdate(&json!({ "pane_id": 1, "scope": ["src/ui/**"] }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["overlaps"], json!([{ "scope": "src/ui/**", "pane_id": 2, "glob": "src/ui/**" }]));
        let r = cockpitPaneUpdate(&json!({ "pane_id": 1, "scope": ["src/lib/**", "README.md"] }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let claims: i64 = c.query_row("SELECT COUNT(*) FROM cockpit_claims WHERE pane_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(claims, 2, "claims follow the scope");
        let r = cockpitPaneUpdate(&json!({ "pane_id": 1, "scope": ["src/lib/**"] }), &c).unwrap();
        assert_eq!(r["ok"], json!(true), "a pane never conflicts with its own claims");
        let claims: i64 = c.query_row("SELECT COUNT(*) FROM cockpit_claims WHERE pane_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(claims, 1);
    }
}
