//! cockpit-promote.rs — twin of skills/cockpit/cockpit-promote.mjs (db lane, EPIC-VIBE-COCKPIT).
//! NAMES ARE CONTRACT: default `cockpitPromote`. Hands a pane's work to the autonomous driver:
//! creates a backlog card capturing the pane's mission + scope + worktree branch + operator
//! guidance, so the gated pipeline can pick it up.
//!
//! `context.db` missing → JS throws `err.contextMissing('cockpit-promote', 'db')`. Unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention as every
//! sibling db-lane twin).
//!
//! NOT parity-deferred in the usual sense: the JS wraps its `await aiCall(...)` in a `try { }
//! catch { /* AI unavailable → metadata fallback */ }` — so unlike book-run.rs/pl-research.rs
//! (which propagate aiCall's Err via `?`), this function SWALLOWS the deferred stub's Err and
//! keeps going down the fully-deterministic metadata-fallback path (matching the exact behavior
//! JS exhibits whenever the AI call fails/is unavailable — the only difference for a SUCCEEDING
//! real AI call is the drafted `card_md` text, which is inherently non-deterministic prose this
//! twin cannot replicate; the deterministic assembly around it — guards, pane/session lookup,
//! scope/context rendering, `ensurePromotePrompt` ensure-step, the metadata-derived
//! title/body/slug, `cardAdd`, and the best-effort blackboard note — is ported faithfully and IS
//! this twin's fully fixture-reachable surface). `ensurePromotePrompt` reuses the
//! already-converted `ai_prompt_find`/`ai_prompt_add` twins (mirrors book-run.rs's
//! `ensure_faculty`). `ensureCockpitSchema` reused via the already-converted
//! `cockpit_cockpit_schema` twin. `cardAdd` reused via the already-converted `entities_card_add`
//! twin.
//!
//! Frozen clock: `Date.now()` → `1767225600000`; `(1767225600000).toString(36)` → the literal
//! `"mjuohs00"` (verified via `node -e`) — hardcoded into the slug the same way other twins
//! hardcode the frozen ISO/epoch literals.
use rusqlite::Connection;
use serde_json::{json, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
} // = JS String(v ?? '')
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
} // = JS String(v)
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
} // = JS `${v}`
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
} // = JS Number(v)
fn is_int(f: f64) -> bool { f.is_finite() && f.fract() == 0.0 }
// = JS Number.prototype.toString()/`${f}` for a raw f64 not sourced from JSON (playbook helper,
// used for `${paneId}` where paneId = Number(input.pane_id) may be non-integer).
fn js_number_to_string(f: f64) -> String {
    if f.is_nan() { return "NaN".to_string(); }
    if f == 0.0 { return "0".to_string(); }
    let neg = f.is_sign_negative();
    let abs = f.abs();
    if abs.is_infinite() { return if neg { "-Infinity".into() } else { "Infinity".into() }; }
    let sci = format!("{:e}", abs);
    let (mantissa, exp_str) = sci.split_once('e').unwrap();
    let exp: i32 = exp_str.parse().unwrap();
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let body = if (-6..=20).contains(&exp) {
        if exp >= 0 {
            if exp + 1 >= k { format!("{digits}{}", "0".repeat((exp + 1 - k) as usize)) }
            else { let (i, f) = digits.split_at((exp + 1) as usize); format!("{i}.{f}") }
        } else { format!("0.{}{digits}", "0".repeat((-exp - 1) as usize)) }
    } else {
        let m = if k == 1 { digits.clone() } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        format!("{m}e{}{}", if exp >= 0 { "+" } else { "-" }, exp.abs())
    };
    if neg { format!("-{body}") } else { body }
}
// Array.prototype.join(', ') semantics: null/undefined elements render as '' (not "null").
fn array_join(a: &[Value], sep: &str) -> String {
    a.iter().map(|v| if v.is_null() { String::new() } else { js_string(v) }).collect::<Vec<_>>().join(sep)
}
fn first(rows: &Value) -> Option<Value> { rows.as_array().and_then(|a| a.first()).cloned() }

const PROMOTE_PROMPT: &str = "cockpit.promote.v1";
const PROMOTE_SYSTEM: &str = "You turn a vibe-cockpit pane's work into a crisp backlog card for an autonomous build pipeline. Be concrete and actionable.";
const PROMOTE_TEMPLATE: &str = "THE OPERATOR'S GOAL for promoting this pane to a card:\n{{goal}}\n\nPANE CONTEXT (the agent, its scope, its worktree branch, the session mission):\n{{context}}\n\nWrite a backlog card in markdown: line 1 is the title prefixed '# '; then a short body — what to build, a few acceptance criteria, and a note that the in-progress work lives on the worktree branch. Follow the operator's GOAL.";

// try promptFind; on miss, promptAdd (mirrors JS ensurePromotePrompt's try/catch — JS does NOT
// re-promptFind after the add, unlike book-run.rs's ensure_faculty / pl-research.rs's ensure_prompt).
fn ensure_promote_prompt(db: &Connection) {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMOTE_PROMPT }), db);
    if existing.is_err() || matches!(&existing, Ok(v) if v.is_null()) {
        let _ = crate::ai_prompt_add::promptAdd(
            &json!({
                "name": PROMOTE_PROMPT,
                "template_md": PROMOTE_TEMPLATE,
                "system_md": PROMOTE_SYSTEM,
                "default_tier": "low",
                "default_temperature": "cold",
                "phase_flow": "single_only",
                "max_output_tokens": 1500,
                "output_fields": [{ "field_name": "card_md", "field_type": "string" }],
            }),
            db,
        );
    }
}

/// JS `Number.prototype.toString(36)` for a non-negative integer — lowercase digits then letters.
fn to_base36(mut n: i64) -> String {
    if n <= 0 { return "0".to_string(); }
    const D: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while n > 0 {
        out.push(D[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("base36 digits are ascii")
}

pub fn cockpitPromote(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    let pane_id_num = js_number(input.get("pane_id"));
    if pane_id_num == 0.0 || pane_id_num.is_nan() {
        return Err("cockpit-promote: pane_id is required".to_string());
    }
    let pane_id_param = if is_int(pane_id_num) { json!(pane_id_num as i64) } else { json!(pane_id_num) };

    let pane_rows = crate::db_json::query_rows(
        db,
        "SELECT p.*, s.name AS session_name, s.mission_md FROM cockpit_panes p JOIN cockpit_sessions s ON s.id = p.session_id WHERE p.id = ?",
        &[pane_id_param],
    )?;
    let Some(pane) = first(&pane_rows) else {
        return Ok(json!({ "ok": false, "error": format!("pane {} not found", js_number_to_string(pane_id_num)) }));
    };

    // `(() => { try { return JSON.parse(pane.scope_json) || []; } catch { return []; } })()` —
    // a parse failure OR a falsy parsed value (null/0/''/false) yields []; a truthy non-array
    // value is kept as-is but is unreachable by `scope.length ? … : …` below (undefined is falsy).
    let scope: Value = match pane.get("scope_json").and_then(Value::as_str) {
        Some(s) => match serde_json::from_str::<Value>(s) {
            Ok(v) if truthy(Some(&v)) => v,
            _ => json!([]),
        },
        None => json!([]),
    };
    let scope_line = match scope.as_array() {
        Some(a) if !a.is_empty() => format!("Scope: {}", array_join(a, ", ")),
        _ => "Scope: (unrestricted)".to_string(),
    };

    let mission_line = format!(
        "Session mission: {}",
        if truthy(pane.get("mission_md")) { str_qq(pane.get("mission_md")) } else { "(none)".to_string() }
    );
    let base_suffix = if truthy(pane.get("base_agent")) { format!(" (base {})", tmpl(pane.get("base_agent"))) } else { String::new() };
    let agent_line = format!("Agent: {}{}", tmpl(pane.get("subagent_name")), base_suffix);
    let branch_line = if truthy(pane.get("branch")) { format!("Worktree branch: {}", tmpl(pane.get("branch"))) } else { String::new() };
    let guidance_line = if truthy(pane.get("extra_prompt_md")) { format!("Operator guidance: {}", tmpl(pane.get("extra_prompt_md"))) } else { String::new() };
    let context_str = [mission_line, agent_line, scope_line, branch_line, guidance_line]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    let session_name = tmpl(pane.get("session_name"));
    let subagent_name = tmpl(pane.get("subagent_name"));

    let title_present = input.get("title").map(|v| !v.is_null()).unwrap_or(false);
    let body_present = input.get("body").map(|v| !v.is_null()).unwrap_or(false);

    let title: String;
    let body: String;
    let mut ai_body: Option<String> = None;
    if title_present || body_present {
        let default_title = format!("[vibe] {subagent_name}: {session_name}");
        let raw_title = if truthy(input.get("title")) { str_qq(input.get("title")) } else { default_title };
        title = raw_title.chars().take(160).collect();
        body = if truthy(input.get("body")) { str_qq(input.get("body")) } else { String::new() };
    } else {
        let mut ai_title: Option<String> = None;
        if truthy(input.get("goal")) {
            ensure_promote_prompt(db);
            let goal_val: String = str_qq(input.get("goal")).chars().take(4000).collect();
            let ai_input = json!({
                "prompt_name": PROMOTE_PROMPT,
                "tier": "low",
                "input_kv": [
                    { "key": "goal", "value": goal_val },
                    { "key": "context", "value": context_str },
                ],
            });
            // PARITY-DEFERRED point: aiCall always returns Err from the deferred stub — JS's
            // `try { … } catch { /* AI unavailable → metadata fallback */ }` swallows it here,
            // so this twin, like the real JS on any AI failure, falls through to the fallback.
            if let Ok(res) = crate::ai_ai_call::aiCall(&ai_input, db) {
                let md = res.get("output_kv").and_then(Value::as_array).and_then(|rows| {
                    rows.iter().find(|o| {
                        o.get("field_name").and_then(Value::as_str) == Some("card_md")
                            && o.get("parent_output_id").map(|v| v.is_null()).unwrap_or(true)
                    })
                }).and_then(|o| o.get("value_text").cloned()).filter(|v| !v.is_null());
                let md_str = md.map(|v| str_qq(Some(&v))).unwrap_or_default();
                if !md_str.trim().is_empty() {
                    let trimmed = md_str.trim().to_string();
                    // `.match(/^#\s+(.+)$/m)` — first line starting with '#' + ONE-OR-MORE
                    // whitespace + at-least-1-char capture; `m[1].trim()` afterward (may yield
                    // '' if the captured text was whitespace-only — kept as Some("") here, same
                    // as JS's aiTitle = '', so the `aiTitle || default` fallback below applies
                    // uniformly to both "no match" and "matched but blank").
                    ai_title = trimmed.lines().find_map(|line| {
                        let rest = line.strip_prefix('#')?;
                        let stripped = rest.trim_start();
                        if stripped.len() == rest.len() { return None; } // no \s+ after '#'
                        if stripped.is_empty() { return None; } // (.+) needs >=1 char
                        Some(stripped.trim().to_string())
                    });
                    ai_body = Some(trimmed);
                }
            }
        }
        let default_title = format!("[vibe] {subagent_name}: {session_name}");
        // `aiTitle || defaultTitle` — a Some("") (matched-but-blank) falls through to default too.
        let raw_title = match &ai_title {
            Some(t) if !t.is_empty() => t.clone(),
            _ => default_title,
        };
        title = raw_title.chars().take(160).collect();
        body = match &ai_body {
            Some(ab) => {
                let idx = tmpl(pane.get("idx"));
                let branch = if truthy(pane.get("branch")) { tmpl(pane.get("branch")) } else { "n/a".to_string() };
                format!("{ab}\n\n---\nFrom the vibe cockpit · pane {idx} ({subagent_name}) · branch {branch}")
            }
            None => {
                let idx = tmpl(pane.get("idx"));
                let intro = format!("Promoted from the vibe cockpit (session \"{session_name}\", pane {idx} / {subagent_name}).");
                let goal_line = if truthy(input.get("goal")) {
                    let g: String = str_qq(input.get("goal")).chars().take(2000).collect();
                    format!("\nGOAL:\n{g}")
                } else {
                    String::new()
                };
                [intro, goal_line, context_str.clone()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n")
            }
        };
    }

    // DRAFT: return the card for the operator to review / edit / approve — do NOT create it yet.
    if truthy(input.get("draft")) {
        return Ok(json!({ "ok": true, "draft": true, "title": title, "body": body }));
    }

    let session_id = tmpl(pane.get("session_id"));
    let idx = tmpl(pane.get("idx"));
    // JS: `Date.now().toString(36)` — the slug's uniqueness suffix. Hardcoding the frozen
    // instant's base36 ("mjuohs00") made every promotion of a pane mint the SAME slug, so the
    // second one collided forever rather than only inside one millisecond.
    let slug = format!("cockpit-{session_id}-{idx}-{}", to_base36(crate::clock::now_ms()));

    let card_input = json!({ "slug": slug, "title": title, "status": "backlog", "body": body });
    if let Err(e) = crate::entities_card_add::cardAdd(&card_input, db) {
        let msg: String = e.chars().take(160).collect();
        return Ok(json!({ "ok": false, "error": format!("card add failed: {msg}") }));
    }

    let note = format!("↗ promoted pane {idx} ({subagent_name}) to driver card {slug}");
    let _ = crate::db_json::run_write(
        db,
        "INSERT INTO cockpit_blackboard (session_id, pane_id, kind, body_md) VALUES (?, NULL, ?, ?)",
        &[pane.get("session_id").cloned().unwrap_or(Value::Null), json!("handoff"), json!(note)],
    );

    Ok(json!({ "ok": true, "slug": slug, "title": title }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT UNIQUE, title TEXT, status TEXT, body_json TEXT, parent_slug TEXT);
             CREATE TABLE cockpit_sessions (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'active', mission_md TEXT, base_branch TEXT NOT NULL DEFAULT '', base_sha TEXT NOT NULL DEFAULT '', mode TEXT NOT NULL DEFAULT 'steer', created_at TEXT, updated_at TEXT);
             CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL, idx INTEGER NOT NULL, subagent_name TEXT NOT NULL DEFAULT '', scope_json TEXT NOT NULL DEFAULT '[]', grants_json TEXT NOT NULL DEFAULT '[]', worktree_path TEXT, branch TEXT, tier TEXT NOT NULL DEFAULT 'low', base_agent TEXT, extra_prompt_md TEXT, model_id TEXT, calls INTEGER NOT NULL DEFAULT 0, cost_usd REAL NOT NULL DEFAULT 0, status TEXT NOT NULL DEFAULT 'idle', last_activity TEXT);
             CREATE TABLE cockpit_blackboard (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL, pane_id INTEGER, kind TEXT NOT NULL DEFAULT 'note', body_md TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL DEFAULT (datetime('now')));
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    fn a_pane(c: &Connection, scope: &str, branch: Option<&str>, base_agent: Option<&str>, extra: Option<&str>) -> (i64, i64) {
        c.execute("INSERT INTO cockpit_sessions (name, mission_md) VALUES ('sess1', 'ship the thing')", []).unwrap();
        let sid = c.last_insert_rowid();
        c.execute(
            "INSERT INTO cockpit_panes (session_id, idx, subagent_name, scope_json, branch, base_agent, extra_prompt_md) VALUES (?, 0, 'builder', ?, ?, ?, ?)",
            rusqlite::params![sid, scope, branch, base_agent, extra],
        ).unwrap();
        (sid, c.last_insert_rowid())
    }

    #[test]
    fn pane_id_required_and_zero_and_nan_are_rejected() {
        let c = setup();
        assert_eq!(cockpitPromote(&json!({}), &c).unwrap_err(), "cockpit-promote: pane_id is required");
        assert_eq!(cockpitPromote(&json!({ "pane_id": 0 }), &c).unwrap_err(), "cockpit-promote: pane_id is required");
        assert_eq!(cockpitPromote(&json!({ "pane_id": "not-a-number" }), &c).unwrap_err(), "cockpit-promote: pane_id is required");
    }

    #[test]
    fn pane_not_found_returns_ok_false() {
        let c = setup();
        let r = cockpitPromote(&json!({ "pane_id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "pane 999 not found" }));
    }

    #[test]
    fn explicit_title_and_body_are_used_verbatim_and_card_is_created() {
        let c = setup();
        let (_sid, pid) = a_pane(&c, "[]", None, None, None);
        let r = cockpitPromote(&json!({ "pane_id": pid, "title": "My Title", "body": "My Body" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["title"], json!("My Title"));
        let card_title: String = c.query_row("SELECT title FROM cards WHERE slug = ?", [r["slug"].as_str().unwrap()], |row| row.get(0)).unwrap();
        assert_eq!(card_title, "My Title");
        let bb_kind: String = c.query_row("SELECT kind FROM cockpit_blackboard LIMIT 1", [], |row| row.get(0)).unwrap();
        assert_eq!(bb_kind, "handoff");
    }

    #[test]
    fn draft_true_returns_without_creating_a_card() {
        let c = setup();
        let (_sid, pid) = a_pane(&c, "[]", None, None, None);
        let r = cockpitPromote(&json!({ "pane_id": pid, "title": "T", "body": "B", "draft": true }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "draft": true, "title": "T", "body": "B" }));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM cards", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn no_title_no_body_no_goal_falls_back_to_metadata_body_with_full_context() {
        let c = setup();
        let (_sid, pid) = a_pane(&c, r#"["src/ui/**","src/api/**"]"#, Some("antiloki/cockpit/1/0"), Some("dev"), Some("be careful"));
        let r = cockpitPromote(&json!({ "pane_id": pid }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["title"], json!("[vibe] builder: sess1"));
        let slug = r["slug"].as_str().unwrap().to_string();
        let body: String = c.query_row("SELECT json_extract(body_json,'$') FROM cards WHERE slug = ?", [&slug], |row| row.get(0)).unwrap();
        // body_json stores a JSON string scalar; json_extract(...,'$') unwraps it to the plain
        // string, so embedded quotes are literal, not backslash-escaped (verified via node -e
        // JSON.stringify round-trip).
        assert!(body.contains("Promoted from the vibe cockpit (session \"sess1\", pane 0 / builder)."));
        assert!(body.contains("Session mission: ship the thing"));
        assert!(body.contains("Agent: builder (base dev)"));
        assert!(body.contains("Scope: src/ui/**, src/api/**"));
        assert!(body.contains("Worktree branch: antiloki/cockpit/1/0"));
        assert!(body.contains("Operator guidance: be careful"));
    }

    #[test]
    fn empty_scope_renders_unrestricted() {
        let c = setup();
        let (_sid, pid) = a_pane(&c, "[]", None, None, None);
        let r = cockpitPromote(&json!({ "pane_id": pid }), &c).unwrap();
        let slug = r["slug"].as_str().unwrap().to_string();
        let body: String = c.query_row("SELECT json_extract(body_json,'$') FROM cards WHERE slug = ?", [&slug], |row| row.get(0)).unwrap();
        assert!(body.contains("Scope: (unrestricted)"));
    }

    #[test]
    fn goal_present_reaches_ensure_prompt_then_falls_back_after_deferred_aicall_fails() {
        // proves the deterministic surface (schema ensure, pane lookup, context assembly,
        // ensure_promote_prompt) ran, aiCall's deferred stub was swallowed (not propagated), and
        // the function fell through cleanly to the metadata-fallback body (mirrors real JS
        // behavior on any AI failure).
        let c = setup();
        let (_sid, pid) = a_pane(&c, "[]", None, None, None);
        let r = cockpitPromote(&json!({ "pane_id": pid, "goal": "ship feature X" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["title"], json!("[vibe] builder: sess1"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'cockpit.promote.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
        let slug = r["slug"].as_str().unwrap().to_string();
        let body: String = c.query_row("SELECT json_extract(body_json,'$') FROM cards WHERE slug = ?", [&slug], |row| row.get(0)).unwrap();
        assert!(body.contains("GOAL:\nship feature X"));
    }

    #[test]
    fn slug_uses_frozen_clock_base36_suffix() {
        let c = setup();
        let (_sid, pid) = a_pane(&c, "[]", None, None, None);
        let r = cockpitPromote(&json!({ "pane_id": pid, "title": "T", "body": "B" }), &c).unwrap();
        assert!(r["slug"].as_str().unwrap().ends_with("-mjuohs00"));
    }

    #[test]
    fn card_add_failure_is_caught_and_reported_as_ok_false() {
        let c = setup();
        let (sid, pid) = a_pane(&c, "[]", None, None, None);
        // pre-insert a card with the exact slug this run will produce, forcing a UNIQUE failure.
        let idx = 0;
        let slug = format!("cockpit-{sid}-{idx}-mjuohs00");
        c.execute("INSERT INTO cards (slug, title, status) VALUES (?, 'dup', 'backlog')", [&slug]).unwrap();
        let r = cockpitPromote(&json!({ "pane_id": pid, "title": "T", "body": "B" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().starts_with("card add failed: "));
    }
}
