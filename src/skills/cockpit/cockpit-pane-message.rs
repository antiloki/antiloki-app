//! cockpit-pane-message.rs — twin of skills/cockpit/cockpit-pane-message.mjs. NAMES ARE CONTRACT:
//! `parseEditsFromReply` · `gatherScopedFiles` · `processEdits` · `cockpitPaneMessage` (default).
//! EPIC-VIBE-COCKPIT P1/P2 — drive ONE pane's agent for a turn: read its memory slices + in-scope
//! files, propose edits, apply them to the pane's WORKTREE (only in-scope files, only with the
//! 'edit' grant), log the turn to the blackboard, post a shared coordination handoff on a write.
//!
//! BESPOKE-NATIVE (real fs directory walk + real file read/write against the pane's worktree,
//! plus an `aiCall` dispatch), not conform-db: proven below via `#[cfg(test)]` against real temp
//! dirs + an in-memory sqlite db (brain attached), same class as atlas-ingest.rs / auth-sign.rs.
//!
//! PARITY-DEFERRED (disclosed, not silently dropped):
//! 1. **`runPaneAgent`'s `aiCall` dispatch.** Reuses the already-converted `ai_ai_call::aiCall`
//!    stub, which is itself PARITY-DEFERRED (a real network BYOK round-trip — see that module's
//!    doc). This twin ports `ensurePanePrompt` (via `ai_prompt_find`/`ai_prompt_add`), the full
//!    persona/mission/coordination/scratch/scope/files assembly (via `entities_agent_persona_resolve
//!    ::resolvePersona`, `cockpit_cockpit_blackboard_read::cockpitBlackboardRead`,
//!    `cockpit__helpers_context_brief::{resolveContextDocs, missionValue, parseExcludes}`,
//!    `gatherScopedFiles` below), and the input_kv shape passed to `aiCall` — everything up to the
//!    point a real model reply would need parsing. Because the stub always `Err`s, this mirrors
//!    the JS `try { ... } catch (e) { agentError = ...; edits = []; }` guard in
//!    `cockpitPaneMessage` exactly: a real model call fails identically in both languages' shape
//!    (an agent_error surfaces, edits stays empty), even though the REASON differs (real model
//!    error vs. deferred-stub error). `parseEditsFromReply` itself — the reply-parsing contract —
//!    is ported byte-faithfully and is independently unit-tested here since it needs no live call.
//! 2. **The `vibeChatOn` dynamic-import branch** (`await import('./cockpit-pane-turn.mjs')`).
//!    `cockpit-pane-turn.mjs` has NO Rust twin yet (a genuinely separate skill, not a stub-only
//!    boundary like aiCall). Per playbook rule 11 ("if the JS imports a skill that is NOT yet a
//!    twin, stop / defer that part"), this twin still computes `vibeChatOn` faithfully (env var
//!    then `cost_settings.vibe_chat_mode`, default 'on') but returns a clear PARITY-DEFERRED `Err`
//!    at the exact dispatch point instead of silently falling through to the legacy path — the
//!    legacy (non-chat-engine) path below it is ported in full and is what every test here
//!    exercises by forcing `ANTILOKI_VIBE_CHAT=off`.
//! Everything else — plan-then-apply (`apply_pending`/`discard_pending`), the edit boundary
//! (`processEdits`, reused for BOTH auto-apply and PLAN paths), blackboard note posts, the
//! unparsed-edit-hint, displayReply stripping, @mention handoff routing, and the ai-call cost
//! meter — is ported faithfully.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } }
fn truthy_num(n: f64) -> bool { n != 0.0 && !n.is_nan() }
fn num_param(n: f64) -> Value { if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) } }
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }

const PROMPT: &str = "cockpit.pane.v4";
const SYSTEM: &str = "You are ONE agent in a multi-agent vibe-coding cockpit. You own a strict file SCOPE inside a git worktree.\nEdit ONLY files matching your scope. Respect the coordination log (other panes' claims). Make the smallest change that satisfies the request.";
const TEMPLATE: &str = r#"YOUR PERSONA (your base identity + the operator's extra guidance for this session):
{{persona}}

MISSION (shared, read-only):
{{mission_md}}

COORDINATION LOG (other panes — respect their claims):
{{coordination}}

YOUR PRIVATE NOTES:
{{scratch}}

YOUR SCOPE (edit ONLY files matching these globs):
{{scope}}

CURRENT IN-SCOPE FILES:
{{files}}

CONTEXT PACK (antiloki's measured view of your scope — weak axes, violated laws with lines, importers, tests; read before you explore):
{{context_pack}}

OPERATOR MESSAGE:
{{user_input}}

HOW TO EDIT FILES (the ONLY way your changes become real):
If the operator asks you to create, add, change, or remove ANY code or file content, you MUST end your reply with ONE fenced code block tagged json containing a JSON array of {"path","content"} — the FULL new content of each touched file, path relative to the worktree, inside your scope. A conversational answer without this block changes NOTHING and is a failed turn.
Example — adding a heading to src/App.jsx would end with:
```json
[{"path":"src/App.jsx","content":"<the ENTIRE file with your change applied>"}]
```
Questions/discussion that touch no files: reply normally, no block."#;

fn ensure_pane_prompt(db: &Connection) {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(Value::as_str) == Some(TEMPLATE) { return; }
    }
    let _ = crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT, "template_md": TEMPLATE, "system_md": SYSTEM,
            "default_tier": "low", "default_temperature": "cold", "phase_flow": "single_only", "max_output_tokens": 4000,
            "output_fields": [{ "field_name": "reply_md", "field_type": "string" }],
        }),
        db,
    );
}

// minimal, dependency-free glob: ** → any, * → segment. Used for scope enforcement (the security boundary).
fn glob_to_re(g: &str) -> regex::Regex {
    let mut pat = String::from("^");
    let mut chars = g.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '*' {
            if chars.peek() == Some(&'*') {
                chars.next();
                pat.push_str(".*");
            } else {
                pat.push_str("[^/]*");
            }
        } else if ".+^${}()|[]\\".contains(c) {
            pat.push('\\');
            pat.push(c);
        } else {
            pat.push(c);
        }
    }
    pat.push('$');
    regex::Regex::new(&pat).unwrap_or_else(|_| regex::Regex::new("$^").unwrap())
}
fn in_scope(rel: &str, scope: &[String]) -> bool { scope.is_empty() || scope.iter().any(|g| glob_to_re(g).is_match(rel)) }
fn safe_rel(p: Option<&Value>) -> Option<String> {
    let s = str_qq(p).replace('\\', "/");
    let s = s.strip_prefix("./").unwrap_or(&s).to_string();
    if s.is_empty() || s.starts_with('/') || s.contains("..") { return None; }
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' { return None; }
    Some(s)
}
// JSON.parse(s ?? '[]') catch → [] ; non-array → []
fn parse_arr(s: Option<&Value>) -> Vec<String> {
    let raw = match s { Some(Value::String(t)) => t.clone(), None | Some(Value::Null) => "[]".to_string(), Some(o) => o.to_string() };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Array(a)) => a.iter().map(|v| str_qq(Some(v))).collect(),
        _ => Vec::new(),
    }
}

fn as_edits(v: &Value) -> Option<Vec<Value>> {
    let arr: Vec<Value> = match v {
        Value::Array(a) => a.clone(),
        Value::Object(_) => vec![v.clone()],
        _ => return None,
    };
    if arr.is_empty() { return None; }
    let all_ok = arr.iter().all(|x| {
        x.is_object() && x.get("path").map_or(false, Value::is_string) && x.get("content").map_or(false, Value::is_string)
    });
    if all_ok { Some(arr) } else { None }
}

/// `export function parseEditsFromReply(text)`. Pulls file edits out of a model reply: (1) a
/// fenced block parsing to an array of {path,content}, tried LAST block first; (2) a fenced block
/// parsing to a single {path,content} object; (3) the whole reply as bare JSON; (4) an embedded
/// balanced-brace `{"path": ...}` object anywhere in the text.
pub fn parseEditsFromReply(text: &Value) -> Value {
    let s = str_qq(Some(text));
    let fence_re = regex::Regex::new(r"(?s)```[a-zA-Z]*\s*(.*?)```").unwrap();
    let blocks: Vec<String> = fence_re.captures_iter(&s).map(|c| c[1].trim().to_string()).collect();
    for inner in blocks.iter().rev() {
        if let Ok(v) = serde_json::from_str::<Value>(inner) {
            if let Some(edits) = as_edits(&v) { return json!(edits); }
        }
    }
    if let Ok(v) = serde_json::from_str::<Value>(s.trim()) {
        if let Some(edits) = as_edits(&v) { return json!(edits); }
    }
    // embedded object: balanced-brace scan from each `{` that leads with a "path" key
    let path_lead = regex::Regex::new(r#"^\{\s*"path"\s*:"#).unwrap();
    let chars: Vec<char> = s.chars().collect();
    let mut start = 0usize;
    while let Some(rel) = chars[start..].iter().position(|&c| c == '{') {
        let idx = start + rel;
        let peek_end = (idx + 40).min(chars.len());
        let peek: String = chars[idx..peek_end].iter().collect();
        if !path_lead.is_match(&peek) { start = idx + 1; continue; }
        let mut depth = 0i32;
        let mut in_str = false;
        let mut esc = false;
        let mut closed_at: Option<usize> = None;
        for i in idx..chars.len() {
            let ch = chars[i];
            if esc { esc = false; continue; }
            if ch == '\\' { esc = true; continue; }
            if ch == '"' { in_str = !in_str; }
            if in_str { continue; }
            if ch == '{' { depth += 1; }
            if ch == '}' {
                depth -= 1;
                if depth == 0 {
                    closed_at = Some(i);
                    break;
                }
            }
        }
        if let Some(end) = closed_at {
            let candidate: String = chars[idx..=end].iter().collect();
            if let Ok(v) = serde_json::from_str::<Value>(&candidate) {
                if let Some(edits) = as_edits(&v) { return json!(edits); }
            }
            break; // JS `break`s the outer for-loop once a balanced object was found (parse fail or not)
        } else {
            break;
        }
    }
    json!([])
}

const SKIP_DIRS: [&str; 5] = ["node_modules", ".git", "dist", "build", ".antiloki"];

/// `export function gatherScopedFiles(root, scope, cap = 40)`. Real recursive directory walk
/// (deterministic: entries sorted by name — `std::fs::read_dir` order is not guaranteed the way
/// JS `readdirSync` happens to be, same disclosed approximation as atlas-ingest.rs).
pub fn gatherScopedFiles(root: &Value, scope: &Value, cap: &Value) -> Value {
    let root_s = str_qq(Some(root));
    let scope_v: Vec<String> = scope.as_array().map(|a| a.iter().map(|v| str_qq(Some(v))).collect()).unwrap_or_default();
    let cap_n = match cap { Value::Null => 40usize, v => js_number(Some(v)).max(0.0) as usize };
    let mut out: Vec<String> = Vec::new();
    walk_scoped(std::path::Path::new(&root_s), std::path::Path::new(&root_s), &scope_v, cap_n, &mut out);
    let joined = out.join("\n\n");
    let joined = take_chars(&joined, 120000);
    json!(if joined.is_empty() { "(no files in scope yet)".to_string() } else { joined })
}
fn walk_scoped(root: &std::path::Path, dir: &std::path::Path, scope: &[String], cap: usize, out: &mut Vec<String>) {
    if out.len() >= cap { return; }
    let mut ents: Vec<std::fs::DirEntry> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return,
    };
    ents.sort_by_key(|e| e.file_name());
    for e in ents {
        if out.len() >= cap { return; }
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') { continue; }
        let abs = e.path();
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            if !SKIP_DIRS.contains(&name.as_str()) { walk_scoped(root, &abs, scope, cap, out); }
            continue;
        }
        let rel = pathdiff_to_slash(root, &abs);
        if !in_scope(&rel, scope) { continue; }
        let mut body = String::new();
        if let Ok(meta) = std::fs::metadata(&abs) {
            if meta.len() <= 8000 {
                if let Ok(b) = std::fs::read(&abs) { body = String::from_utf8_lossy(&b).to_string(); }
            }
        }
        out.push(format!("--- {rel} ---\n{}", take_chars(&body, 4000)));
    }
}
fn pathdiff_to_slash(root: &std::path::Path, abs: &std::path::Path) -> String {
    let rel = abs.strip_prefix(root).unwrap_or(abs);
    rel.to_string_lossy().replace('\\', "/")
}

fn run_pane_agent(pane: &Value, user_text: &str, scope: &[String], db: &Connection) -> Result<(String, Value, Option<i64>), String> {
    ensure_pane_prompt(db);
    let base_agent = pane.get("base_agent").and_then(Value::as_str).unwrap_or("");
    let mut persona = String::new();
    if !base_agent.is_empty() {
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
    let slices = crate::cockpit_cockpit_blackboard_read::cockpitBlackboardRead(&json!({ "session_id": session_id, "pane_id": pane_id }), db)?;
    let coordination = slices.get("coordination").and_then(Value::as_array).map(|a| {
        a.iter().map(|c| format!("- [{}] {}", str_qq(c.get("kind")), str_qq(c.get("body_md")))).collect::<Vec<_>>().join("\n")
    }).unwrap_or_default();
    let coordination = if coordination.is_empty() { "(none)".to_string() } else { take_chars(&coordination, 8000) };
    let scratch = slices.get("scratch").and_then(Value::as_array).map(|a| {
        a.iter().map(|c| format!("- {}", str_qq(c.get("body_md")))).collect::<Vec<_>>().join("\n")
    }).unwrap_or_default();
    let scratch = if scratch.is_empty() { "(none)".to_string() } else { take_chars(&scratch, 4000) };
    let worktree = pane.get("worktree_path").and_then(Value::as_str).filter(|s| !s.is_empty());
    let files = match worktree {
        Some(w) => str_qq(Some(&gatherScopedFiles(&json!(w), &json!(scope), &Value::Null))),
        None => "(worktree not provisioned)".to_string(),
    };
    let excludes = crate::cockpit__helpers_context_brief::parseExcludes(&pane.get("context_excludes_json").cloned().unwrap_or(Value::Null));
    let resolved_docs = crate::cockpit__helpers_context_brief::resolveContextDocs(db, slices.get("context_docs").unwrap_or(&json!([])), &excludes, &json!({ "db": Value::Null }));
    let mission_md = crate::cockpit__helpers_context_brief::missionValue(slices.get("mission_md").unwrap_or(&Value::Null), &resolved_docs);
    // CONTEXT PACK (2026-09-14): an `agent` pane has no file reader, so the baked pack rides inline.
    let context_pack = worktree.and_then(|w| crate::cockpit_cockpit_pane_context_bake::pack_prompt_text(w, crate::cockpit_cockpit_pane_context_bake::PACK_PROMPT_CAP)).unwrap_or_else(|| "(no context pack baked for this pane)".to_string());

    let tier = pane.get("tier").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("low");
    let model_id = pane.get("model_id").and_then(Value::as_str).filter(|s| !s.is_empty());
    let mut ai_input = Map::new();
    ai_input.insert("prompt_name".into(), json!(PROMPT));
    ai_input.insert("tier".into(), json!(tier));
    if let Some(m) = model_id { ai_input.insert("model_id".into(), json!(m)); }
    ai_input.insert("input_kv".into(), json!([
        { "key": "persona", "value": take_chars(&persona, 8000) },
        { "key": "mission_md", "value": mission_md },
        { "key": "coordination", "value": coordination },
        { "key": "scratch", "value": scratch },
        { "key": "scope", "value": if scope.is_empty() { "(unrestricted)".to_string() } else { scope.join("\n") } },
        { "key": "files", "value": files },
        { "key": "context_pack", "value": context_pack },
        { "key": "user_input", "value": user_text },
    ]));
    // PARITY-DEFERRED: crate::ai_ai_call::aiCall is the network-orchestration stub (see module
    // doc §1) — it always Errs, mirroring the JS `catch` path that sets agent_error + edits=[].
    let res = crate::ai_ai_call::aiCall(&Value::Object(ai_input), db)?;
    let reply = res.get("output_kv").and_then(Value::as_array).and_then(|a| {
        a.iter().find(|o| o.get("field_name").and_then(Value::as_str) == Some("reply_md") && o.get("parent_output_id").map_or(true, Value::is_null))
    }).and_then(|o| o.get("value_text")).and_then(Value::as_str).unwrap_or("").to_string();
    let ai_call_id = res.get("ai_call_id").and_then(Value::as_i64);
    let edits = parseEditsFromReply(&json!(reply));
    Ok((reply, edits, ai_call_id))
}

/// `export function processEdits({ pane, edits, scope, canEdit, apply })`. Validates + (with
/// `apply: true`) writes a batch of edits against the pane's boundary; `apply: false` produces a
/// PLAN (per-file verdict + current content) for the FE diff. Exported: this is the ONE
/// enforcement point for both the legacy reply-parsing caller and (in JS) the chat-engine file
/// tools caller.
pub fn processEdits(input: &Value) -> Value {
    let pane = input.get("pane").cloned().unwrap_or(json!({}));
    let scope: Vec<String> = input.get("scope").and_then(Value::as_array).map(|a| a.iter().map(|v| str_qq(Some(v))).collect()).unwrap_or_default();
    let can_edit = truthy(input.get("canEdit"));
    let apply = truthy(input.get("apply"));
    let worktree = pane.get("worktree_path").and_then(Value::as_str).filter(|s| !s.is_empty());
    let edits = input.get("edits").and_then(Value::as_array).cloned().unwrap_or_default();

    let mut applied: Vec<Value> = Vec::new();
    let mut rejected: Vec<Value> = Vec::new();
    let mut plan: Vec<Value> = Vec::new();
    for e in &edits {
        let rel = match safe_rel(e.get("path")) {
            Some(r) => r,
            None => { rejected.push(json!({ "path": e.get("path").cloned().unwrap_or(Value::Null), "reason": "unsafe path" })); continue; }
        };
        if !can_edit { rejected.push(json!({ "path": rel, "reason": "pane has no edit grant" })); continue; }
        if !in_scope(&rel, &scope) { rejected.push(json!({ "path": rel, "reason": "out of pane scope" })); continue; }
        let root = match worktree {
            Some(w) => w,
            None => { rejected.push(json!({ "path": rel, "reason": "worktree not provisioned" })); continue; }
        };
        let abs = std::path::Path::new(root).join(&rel);
        if apply {
            let content = str_qq(e.get("content"));
            let write_res = abs.parent().map(std::fs::create_dir_all).unwrap_or(Ok(())).and_then(|_| std::fs::write(&abs, &content));
            match write_res {
                Ok(_) => applied.push(json!(rel)),
                Err(e2) => rejected.push(json!({ "path": rel, "reason": take_chars(&e2.to_string(), 100) })),
            }
        } else {
            let old = std::fs::read_to_string(&abs).ok();
            plan.push(json!({ "path": rel, "old_content": old, "content": str_qq(e.get("content")) }));
        }
    }
    json!({ "applied": applied, "rejected": rejected, "plan": plan })
}

fn vibe_chat_on(db: &Connection) -> bool {
    let env = std::env::var("ANTILOKI_VIBE_CHAT").unwrap_or_default().to_lowercase();
    if env == "on" || env == "1" || env == "true" { return true; }
    if env == "off" || env == "0" || env == "false" { return false; }
    match crate::db_json::query_rows(db, "SELECT value FROM cost_settings WHERE key = 'vibe_chat_mode'", &[]) {
        Ok(v) => v.as_array().and_then(|a| a.first()).and_then(|r| r.get("value")).and_then(Value::as_str).unwrap_or("on") == "on",
        Err(_) => true,
    }
}

fn insert_blackboard(db: &Connection, session_id: &Value, pane_id: &Value, kind: &str, body_md: &str) {
    let _ = crate::db_json::run_write(db, "INSERT INTO cockpit_blackboard (session_id, pane_id, kind, body_md) VALUES (?,?,?,?)", &[session_id.clone(), pane_id.clone(), json!(kind), json!(body_md)]);
}

pub fn cockpitPaneMessage(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));
    let pane_id_n = js_number(input.get("pane_id"));
    if !truthy_num(pane_id_n) { return Err("cockpit-pane-message: pane_id is required".into()); }
    let pane_id = num_param(pane_id_n);

    let rows = crate::db_json::query_rows(db, "SELECT * FROM cockpit_panes WHERE id = ?", &[pane_id.clone()])?;
    let pane = match rows.as_array().and_then(|a| a.first()) {
        Some(p) => p.clone(),
        None => return Ok(json!({ "ok": false, "error": format!("pane {} not found", pane_id_n as i64) })),
    };
    let session_id = pane.get("session_id").cloned().unwrap_or(Value::Null);

    let user_text = take_chars(&str_qq(input.get("user_input")), 20000);
    let scope = parse_arr(pane.get("scope_json"));
    let grants = parse_arr(pane.get("grants_json"));
    let can_edit = grants.iter().any(|g| g == "edit" || g == "write");

    let op = input.get("op").and_then(Value::as_str).unwrap_or("");
    if op == "apply_pending" || op == "discard_pending" {
        let pending = parse_arr_json(pane.get("pending_edits_json"));
        crate::db_json::run_write(db, "UPDATE cockpit_panes SET pending_edits_json = NULL, last_activity = datetime('now') WHERE id = ?", &[pane_id.clone()])?;
        if op == "discard_pending" || pending.is_empty() {
            insert_blackboard(db, &session_id, &pane_id, "note", &format!("🧑 ✗ plan {}", if op == "discard_pending" { "discarded" } else { "was empty" }));
            let discarded: Vec<Value> = pending.iter().map(|p| p.get("path").cloned().unwrap_or(Value::Null)).collect();
            return Ok(json!({ "ok": true, "pane_id": pane_id_n as i64, "applied": [], "rejected": [], "discarded": discarded }));
        }
        let r = processEdits(&json!({ "pane": pane, "edits": pending, "scope": scope, "canEdit": can_edit, "apply": true }));
        let applied = r.get("applied").and_then(Value::as_array).cloned().unwrap_or_default();
        let rejected = r.get("rejected").and_then(Value::as_array).cloned().unwrap_or_default();
        if !applied.is_empty() {
            let list = applied.iter().map(|v| str_qq(Some(v))).collect::<Vec<_>>().join(", ");
            insert_blackboard(db, &session_id, &pane_id, "note", &format!("✅ applied → {list}"));
            insert_blackboard(db, &session_id, &Value::Null, "handoff", &format!("pane {} ({}) applied: {list}", str_qq(pane.get("idx")), str_qq(pane.get("subagent_name"))));
        }
        return Ok(json!({ "ok": true, "pane_id": pane_id_n as i64, "applied": applied, "rejected": rejected }));
    }

    // EPIC-VIBE-CHAT-UNIFICATION M1: plain agent-pane messages route through the chat engine
    // (cockpit-pane-turn.mjs) — PARITY-DEFERRED, no Rust twin yet (see module doc §2).
    let pane_kind = pane.get("pane_kind").and_then(Value::as_str).unwrap_or("agent");
    if input.get("edits").map_or(true, |v| !v.is_array()) && pane_kind == "agent" && vibe_chat_on(db) {
        return Err("cockpit-pane-message: the vibeChatOn dynamic-import branch (cockpit-pane-turn.mjs) is PARITY-DEFERRED — that skill has no Rust twin yet. Set ANTILOKI_VIBE_CHAT=off (or pass programmatic `edits`) to exercise the ported legacy path.".into());
    }

    crate::db_json::run_write(db, "UPDATE cockpit_panes SET status = 'working', last_activity = datetime('now') WHERE id = ?", &[pane_id.clone()])?;
    if !user_text.is_empty() { insert_blackboard(db, &session_id, &pane_id, "note", &format!("🧑 {user_text}")); }

    let mut reply = str_qq(input.get("reply_md"));
    let mut edits: Value = match input.get("edits") { Some(v) if v.is_array() => v.clone(), _ => Value::Null };
    let mut ai_call_id: Option<i64> = None;
    let mut agent_error: Option<String> = None;
    if edits.is_null() {
        match run_pane_agent(&pane, &user_text, &scope, db) {
            Ok((r, e, id)) => { reply = r; edits = e; ai_call_id = id; }
            Err(e) => { agent_error = Some(take_chars(&e, 200)); edits = json!([]); }
        }
    }
    let edits_arr = edits.as_array().cloned().unwrap_or_default();

    let auto_now = js_number(pane.get("auto_apply")) == 1.0 || input.get("edits").map_or(false, Value::is_array);
    let mut applied: Vec<Value> = Vec::new();
    let mut rejected: Vec<Value> = Vec::new();
    let mut pending_plan: Option<Vec<Value>> = None;
    if auto_now {
        let r = processEdits(&json!({ "pane": pane, "edits": edits_arr, "scope": scope, "canEdit": can_edit, "apply": true }));
        applied = r.get("applied").and_then(Value::as_array).cloned().unwrap_or_default();
        rejected = r.get("rejected").and_then(Value::as_array).cloned().unwrap_or_default();
    } else if !edits_arr.is_empty() {
        let r = processEdits(&json!({ "pane": pane, "edits": edits_arr, "scope": scope, "canEdit": can_edit, "apply": false }));
        rejected = r.get("rejected").and_then(Value::as_array).cloned().unwrap_or_default();
        let plan = r.get("plan").and_then(Value::as_array).cloned().unwrap_or_default();
        if !plan.is_empty() {
            crate::db_json::run_write(db, "UPDATE cockpit_panes SET pending_edits_json = ? WHERE id = ?", &[json!(serde_json::to_string(&plan).unwrap_or_default()), pane_id.clone()])?;
            pending_plan = Some(plan);
        }
    }

    let path_re = regex::Regex::new(r#""path"\s*:\s*""#).unwrap();
    let unparsed_edit_hint = edits_arr.is_empty() && !auto_now && pending_plan.is_none() && path_re.is_match(&reply);
    if let Some(ae) = &agent_error { if reply.is_empty() { reply = format!("agent error: {ae}"); } }

    let mut display_reply = reply.clone();
    if !edits_arr.is_empty() {
        let fence_re = regex::Regex::new(r"(?si)```(?:json)?\s*.*?```").unwrap();
        display_reply = fence_re.replace_all(&display_reply, |caps: &regex::Captures| {
            let m = &caps[0];
            if path_re.is_match(m) { String::new() } else { m.to_string() }
        }).trim().to_string();
        let bare_re = regex::Regex::new(r"^\s*[\[{]").unwrap();
        if bare_re.is_match(&display_reply) && path_re.is_match(&display_reply) { display_reply = String::new(); }
        if display_reply.is_empty() {
            let list = edits_arr.iter().map(|e| str_qq(e.get("path"))).collect::<Vec<_>>().join(", ");
            display_reply = format!("📝 edits → {list}");
        }
    }
    if !display_reply.is_empty() { insert_blackboard(db, &session_id, &pane_id, "note", &format!("🤖 {}", take_chars(&display_reply, 4000))); }
    if unparsed_edit_hint {
        insert_blackboard(db, &session_id, &pane_id, "note", "⚠ the reply contains edit-like JSON that did not match the edit contract — no plan was made. Ask again or rephrase; the agent must emit {\"path\",\"content\"}.");
    }
    if !rejected.is_empty() {
        let list = rejected.iter().map(|r| format!("{} ({})", r.get("path").map(|p| str_qq(Some(p))).filter(|s| !s.is_empty()).unwrap_or_else(|| "?".to_string()), str_qq(r.get("reason")))).collect::<Vec<_>>().join(" · ");
        insert_blackboard(db, &session_id, &pane_id, "note", &format!("⚠ rejected: {list}"));
    }
    if !applied.is_empty() {
        let list = applied.iter().map(|v| str_qq(Some(v))).collect::<Vec<_>>().join(", ");
        let auto_flag = if js_number(pane.get("auto_apply")) == 1.0 { " (auto ⚡)" } else { "" };
        insert_blackboard(db, &session_id, &pane_id, "note", &format!("✅ applied → {list}{auto_flag}"));
        insert_blackboard(db, &session_id, &Value::Null, "handoff", &format!("pane {} ({}) edited: {list}", str_qq(pane.get("idx")), str_qq(pane.get("subagent_name"))));
    }
    if let Some(plan) = &pending_plan {
        let list = plan.iter().map(|p| str_qq(p.get("path"))).collect::<Vec<_>>().join(", ");
        insert_blackboard(db, &session_id, &pane_id, "note", &format!("📋 plan pending — {} file(s): {list} (Apply to write)", plan.len()));
    }

    // @mention handoff: route this turn to a sibling pane named after the mention
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

    if let Some(id) = ai_call_id {
        let mut cost = 0.0;
        if let Ok(rows) = crate::db_json::query_rows(db, "SELECT cost_usd_actual AS c FROM brain.ai_calls WHERE id = ?", &[json!(id)]) {
            cost = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("c")).and_then(Value::as_f64).unwrap_or(0.0);
        }
        let _ = crate::db_json::run_write(db, "UPDATE cockpit_panes SET calls = calls + 1, cost_usd = cost_usd + ? WHERE id = ?", &[json!(cost), pane_id.clone()]);
    }
    let status = if agent_error.is_some() { "error" } else { "idle" };
    crate::db_json::run_write(db, "UPDATE cockpit_panes SET status = ?, last_activity = datetime('now') WHERE id = ?", &[json!(status), pane_id.clone()])?;

    Ok(json!({
        "ok": agent_error.is_none(),
        "pane_id": pane_id_n as i64,
        "reply_md": reply,
        "applied": applied,
        "rejected": rejected,
        "pending": pending_plan,
        "ai_call_id": ai_call_id,
        "agent_error": agent_error,
        "unparsed_edit_hint": unparsed_edit_hint,
    }))
}

fn parse_arr_json(s: Option<&Value>) -> Vec<Value> {
    let raw = match s { Some(Value::String(t)) => t.clone(), None | Some(Value::Null) => "[]".to_string(), Some(o) => o.to_string() };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Array(a)) => a,
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-cockpit-pane-message-rs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
        c.execute(
            "INSERT INTO cockpit_sessions (id, name, created_at) VALUES (1, 's1', datetime('now'))",
            [],
        ).unwrap();
        c
    }

    fn insert_pane(c: &Connection, worktree: &str, scope: &str, grants: &str, auto_apply: i64) -> i64 {
        crate::db_json::run_write(
            c,
            "INSERT INTO cockpit_panes (session_id, idx, subagent_name, worktree_path, scope_json, grants_json, auto_apply, pane_kind) VALUES (1, 0, 'dev', ?, ?, ?, ?, 'agent')",
            &[json!(worktree), json!(scope), json!(grants), json!(auto_apply)],
        ).unwrap().1
    }

    // ── parseEditsFromReply ──────────────────────────────────────────────
    #[test]
    fn parses_fenced_array_and_single_object_and_bare_and_embedded() {
        let arr = parseEditsFromReply(&json!("Here you go:\n```json\n[{\"path\":\"a.txt\",\"content\":\"hi\"}]\n```\n"));
        assert_eq!(arr, json!([{"path":"a.txt","content":"hi"}]));

        let single = parseEditsFromReply(&json!("```json\n{\"path\":\"b.txt\",\"content\":\"yo\"}\n```"));
        assert_eq!(single, json!([{"path":"b.txt","content":"yo"}]));

        let bare = parseEditsFromReply(&json!(" [{\"path\":\"c.txt\",\"content\":\"z\"}] "));
        assert_eq!(bare, json!([{"path":"c.txt","content":"z"}]));

        let embedded = parseEditsFromReply(&json!("noise before {\"path\": \"d.txt\", \"content\": \"w\"} noise after"));
        assert_eq!(embedded, json!([{"path":"d.txt","content":"w"}]));

        let none = parseEditsFromReply(&json!("just a conversational reply"));
        assert_eq!(none, json!([]));
    }

    #[test]
    fn last_fenced_block_wins_when_multiple() {
        let r = parseEditsFromReply(&json!("```json\nnot valid edits\n```\nsome text\n```json\n[{\"path\":\"only.txt\",\"content\":\"x\"}]\n```"));
        assert_eq!(r, json!([{"path":"only.txt","content":"x"}]));
    }

    // ── gatherScopedFiles ─────────────────────────────────────────────────
    #[test]
    fn gathers_only_in_scope_files_sorted() {
        let dir = temp_dir("gather");
        std::fs::write(dir.join("keep.js"), "const x = 1;").unwrap();
        std::fs::write(dir.join("skip.md"), "# nope").unwrap();
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        std::fs::write(dir.join("node_modules/ignored.js"), "ignored").unwrap();
        let out = gatherScopedFiles(&json!(dir.to_string_lossy()), &json!(["*.js"]), &Value::Null);
        let s = out.as_str().unwrap();
        assert!(s.contains("--- keep.js ---"));
        assert!(!s.contains("skip.md"));
        assert!(!s.contains("ignored.js"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_scope_matches_everything_unrestricted_default() {
        let dir = temp_dir("gather-unrestricted");
        std::fs::write(dir.join("any.txt"), "content").unwrap();
        let out = gatherScopedFiles(&json!(dir.to_string_lossy()), &json!([]), &Value::Null);
        assert!(out.as_str().unwrap().contains("any.txt"));
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── processEdits ─────────────────────────────────────────────────────
    #[test]
    fn process_edits_apply_writes_in_scope_rejects_out_of_scope_and_ungranted() {
        let dir = temp_dir("apply");
        let pane = json!({ "worktree_path": dir.to_string_lossy() });
        let r = processEdits(&json!({
            "pane": pane, "scope": ["src/**"], "canEdit": true, "apply": true,
            "edits": [
                { "path": "src/App.jsx", "content": "hello" },
                { "path": "outside.txt", "content": "nope" },
                { "path": "../escape.txt", "content": "nope2" },
            ],
        }));
        assert_eq!(r["applied"], json!(["src/App.jsx"]));
        assert_eq!(std::fs::read_to_string(dir.join("src/App.jsx")).unwrap(), "hello");
        let rejected = r["rejected"].as_array().unwrap();
        assert_eq!(rejected.len(), 2);
        assert_eq!(rejected[0]["reason"], json!("out of pane scope"));
        assert_eq!(rejected[1]["reason"], json!("unsafe path"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn process_edits_no_grant_rejects_all() {
        let dir = temp_dir("nogrant");
        let pane = json!({ "worktree_path": dir.to_string_lossy() });
        let r = processEdits(&json!({ "pane": pane, "scope": [], "canEdit": false, "apply": true, "edits": [{ "path": "a.txt", "content": "x" }] }));
        assert_eq!(r["applied"], json!([]));
        assert_eq!(r["rejected"][0]["reason"], json!("pane has no edit grant"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn process_edits_plan_mode_captures_old_content() {
        let dir = temp_dir("plan");
        std::fs::write(dir.join("existing.txt"), "old body").unwrap();
        let pane = json!({ "worktree_path": dir.to_string_lossy() });
        let r = processEdits(&json!({ "pane": pane, "scope": [], "canEdit": true, "apply": false, "edits": [{ "path": "existing.txt", "content": "new body" }] }));
        let plan = r["plan"].as_array().unwrap();
        assert_eq!(plan[0]["old_content"], json!("old body"));
        assert_eq!(plan[0]["content"], json!("new body"));
        assert!(!dir.join("existing.txt").exists() || std::fs::read_to_string(dir.join("existing.txt")).unwrap() == "old body");
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── cockpitPaneMessage: legacy path (vibe chat off), auto-apply ────────
    #[test]
    fn message_auto_apply_writes_file_and_posts_notes() {
        with_env("ANTILOKI_VIBE_CHAT", Some("off"), || {
            let dir = temp_dir("msg-auto");
            let c = seeded();
            let pane_id = insert_pane(&c, &dir.to_string_lossy(), "[\"**\"]", "[\"edit\"]", 1);
            let r = cockpitPaneMessage(&json!({
                "pane_id": pane_id, "user_input": "add a file",
                "edits": [{ "path": "new.txt", "content": "hi there" }],
            }), &c).unwrap();
            assert_eq!(r["ok"], json!(true));
            assert_eq!(r["applied"], json!(["new.txt"]));
            assert_eq!(std::fs::read_to_string(dir.join("new.txt")).unwrap(), "hi there");
            let notes = crate::db_json::query_rows(&c, "SELECT body_md FROM cockpit_blackboard WHERE pane_id = ? ORDER BY id", &[json!(pane_id)]).unwrap();
            let bodies: Vec<String> = notes.as_array().unwrap().iter().map(|r| r["body_md"].as_str().unwrap().to_string()).collect();
            assert!(bodies.iter().any(|b| b.starts_with("🧑 add a file")));
            assert!(bodies.iter().any(|b| b.starts_with("✅ applied → new.txt")));
            std::fs::remove_dir_all(&dir).ok();
        });
    }

    #[test]
    fn message_pending_plan_when_not_auto_apply() {
        with_env("ANTILOKI_VIBE_CHAT", Some("off"), || {
            let dir = temp_dir("msg-plan");
            let c = seeded();
            let pane_id = insert_pane(&c, &dir.to_string_lossy(), "[]", "[\"edit\"]", 0);
            let r = cockpitPaneMessage(&json!({ "pane_id": pane_id, "edits": [{ "path": "p.txt", "content": "v" }] }), &c);
            // programmatic edits array present -> autoNow true (Array.isArray(input.edits)) even with auto_apply=0
            let r = r.unwrap();
            assert_eq!(r["applied"], json!(["p.txt"]));
            std::fs::remove_dir_all(&dir).ok();
        });
    }

    #[test]
    fn message_pane_not_found_returns_ok_false() {
        let c = seeded();
        let r = cockpitPaneMessage(&json!({ "pane_id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "pane 999 not found" }));
    }

    #[test]
    fn message_apply_pending_writes_and_clears() {
        with_env("ANTILOKI_VIBE_CHAT", Some("off"), || {
            let dir = temp_dir("msg-apply-pending");
            let c = seeded();
            let pane_id = insert_pane(&c, &dir.to_string_lossy(), "[]", "[\"edit\"]", 0);
            let pending = json!([{ "path": "q.txt", "content": "queued" }]);
            crate::db_json::run_write(&c, "UPDATE cockpit_panes SET pending_edits_json = ? WHERE id = ?", &[json!(pending.to_string()), json!(pane_id)]).unwrap();
            let r = cockpitPaneMessage(&json!({ "pane_id": pane_id, "op": "apply_pending" }), &c).unwrap();
            assert_eq!(r["applied"], json!(["q.txt"]));
            assert_eq!(std::fs::read_to_string(dir.join("q.txt")).unwrap(), "queued");
            let row = crate::db_json::query_rows(&c, "SELECT pending_edits_json FROM cockpit_panes WHERE id = ?", &[json!(pane_id)]).unwrap();
            assert_eq!(row[0]["pending_edits_json"], Value::Null);
            std::fs::remove_dir_all(&dir).ok();
        });
    }

    #[test]
    fn message_discard_pending_posts_note_and_clears() {
        with_env("ANTILOKI_VIBE_CHAT", Some("off"), || {
            let c = seeded();
            let pane_id = insert_pane(&c, "", "[]", "[\"edit\"]", 0);
            let pending = json!([{ "path": "q.txt", "content": "x" }]);
            crate::db_json::run_write(&c, "UPDATE cockpit_panes SET pending_edits_json = ? WHERE id = ?", &[json!(pending.to_string()), json!(pane_id)]).unwrap();
            let r = cockpitPaneMessage(&json!({ "pane_id": pane_id, "op": "discard_pending" }), &c).unwrap();
            assert_eq!(r["discarded"], json!(["q.txt"]));
        });
    }

    #[test]
    fn message_deferred_when_vibe_chat_on_and_no_edits() {
        // no ANTILOKI_VIBE_CHAT override, no cost_settings row -> defaults to 'on' -> deferred branch.
        // Guard the read against other tests' with_env mutation of the same process-global var.
        with_env("ANTILOKI_VIBE_CHAT", None, || {
            let c = seeded();
            let pane_id = insert_pane(&c, "", "[]", "[\"edit\"]", 0);
            let r = cockpitPaneMessage(&json!({ "pane_id": pane_id }), &c);
            assert!(r.is_err());
            // this branch returns cockpit-pane-message's OWN PARITY-DEFERRED Err (module doc §2:
            // cockpit-pane-turn has no Rust twin), not an aiCall error.
            let e = r.unwrap_err();
            assert!(e.contains("PARITY-DEFERRED"), "unexpected error: {e}");
        });
    }

    #[test]
    fn message_at_mention_posts_handoff_to_sibling_pane() {
        with_env("ANTILOKI_VIBE_CHAT", Some("off"), || {
            let c = seeded();
            let pane_a = insert_pane(&c, "", "[]", "[\"edit\"]", 0);
            crate::db_json::run_write(&c, "UPDATE cockpit_panes SET subagent_name = 'alice' WHERE id = ?", &[json!(pane_a)]).unwrap();
            let pane_b_id = crate::db_json::run_write(&c, "INSERT INTO cockpit_panes (session_id, idx, subagent_name, worktree_path, scope_json, grants_json, auto_apply, pane_kind) VALUES (1, 1, 'bob', '', '[]', '[]', 0, 'agent')", &[]).unwrap().1;
            let r = cockpitPaneMessage(&json!({ "pane_id": pane_a, "user_input": "@bob please help", "edits": [] }), &c).unwrap();
            assert_eq!(r["ok"], json!(true));
            let notes = crate::db_json::query_rows(&c, "SELECT body_md FROM cockpit_blackboard WHERE pane_id = ?", &[json!(pane_b_id)]).unwrap();
            let bodies: Vec<String> = notes.as_array().unwrap().iter().map(|r| r["body_md"].as_str().unwrap().to_string()).collect();
            assert!(bodies.iter().any(|b| b.contains("from alice")));
        });
    }
}
