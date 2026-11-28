//! cockpit-pane-add.rs — twin of skills/cockpit/cockpit-pane-add.mjs. NAMES ARE CONTRACT:
//! `cockpitPaneAdd` (default). EPIC-VIBE-COCKPIT P1: add a pane to a session — a scoped agent
//! slot bound to its own git worktree. The pane row is always created first; the worktree +
//! engine brief files are provisioned BEST-EFFORT afterward (a failure leaves the pane usable
//! with worktree pending, recorded in `warn`).
//!
//! BESPOKE-NATIVE (not conform-db fixture-tested): `worktreeAdd` shells out to real `git`
//! unconditionally on the provisioning path (same VET rule as its own twin,
//! git-manager-worktree-ops.rs), and the engine-provisioning block does real filesystem writes
//! (AGENTS.md/CLAUDE.md/opencode.json) plus an optional real `git sparse-checkout` subprocess —
//! none of that is fixture-seedable through conform-db's schema/seed format. Proven instead via
//! `#[cfg(test)]` native tests against real temp git repos, mirroring
//! git-manager-worktree-ops.rs / auth-sign.rs.
//!
//! Faithfully-ported JS QUIRK: the session lookup only `SELECT`s `id, mode` — `session.mission_md`
//! (read later while building the pane brief) is therefore ALWAYS `undefined` in production too,
//! so the brief's MISSION line is always the `'(none set)'` fallback. Ported as-is (not "fixed"):
//! this twin never reads a `mission_md` column off the session row either.
//!
//! PARITY-DEFERRED boundary: the JS "build the environment on creation" block
//! (`npm install`/`npm ci`, fire-and-forget via `spawn(...).unref()`, streaming stdout/stderr
//! into `build_gate_live` as it arrives, and a final blackboard note on `child.on('close')`) is
//! genuinely async background work keyed off a `context.db` handle that in JS outlives the
//! request. The Rust standard-lane signature `(input: &Value, db: &Connection)` borrows `db` only
//! for the duration of this call — there is no `'static` connection to hand a background thread,
//! so the streamed-output/close-handler portion cannot be ported without inventing a mechanism
//! the JS source doesn't have. This twin ports the SYNCHRONOUS setup faithfully (the
//! package.json/node_modules existence gate, the initial "⏳ building the environment…" blackboard
//! note, and the first `build_gate_live` line) and then kicks the real `npm install`/`npm ci`
//! process off in a detached, output-discarding fire-and-forget child (real side effect on disk,
//! matching JS's `child.unref()` intent) — but does NOT stream its output back or write the
//! close-handler's completion note, since that requires the borrowed `db` to survive past return.
//! Native tests exercise the synchronous gate (both branches: package.json absent → no-op; a repo
//! with no package.json in the checkout never enters this block) without asserting on the
//! detached process's outcome.
//!
//! Cross-module calls (pre-existing Rust twins): `crate::cockpit_cockpit_schema::ensureCockpitSchema`,
//! `crate::agents_git_manager_git_manager_worktree_ops::worktreeAdd`,
//! `crate::cockpit__helpers_scope::globsOverlap`.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

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
}
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
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
fn num_tmpl(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 { format!("{}", n as i64) } else { n.to_string() }
}
fn id_param(n: f64) -> Value {
    if n.is_finite() && n.fract() == 0.0 && n >= i64::MIN as f64 && n <= i64::MAX as f64 { json!(n as i64) } else { json!(n) }
}
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }

// ROLE_GRANTS: access defaults BY ROLE (dev reads+edits, qa reads+runs, planners/managers read-only).
fn role_grants(base_agent: Option<&str>) -> Vec<&'static str> {
    match base_agent {
        Some("dev") => vec!["read", "edit"],
        Some("qa") => vec!["read", "run"],
        Some("pl") => vec!["read"],
        Some("tl") => vec!["read"],
        Some("man") => vec!["read"],
        _ => vec!["read", "edit", "run"],
    }
}

// THE FIVE SEATS ARE ALL PANE KINDS (2026-09-28). `gemini`, `grok` and `cursor` were missing, and an unknown
// kind is silently rewritten to "agent" — so a fork of a Cursor agent, or a switch onto one, produced a pane whose
// kind was the word "agent", which is not an engine: unstartable, and unswitchable because the switch has to start
// it. Found on pane 50. Keep this list and `serve::engines::ENGINES` in step.
const PANE_KINDS: &[&str] = &[
    "agent", "opencode", "opencode-direct", "claude-code", "codex", "gemini", "grok", "cursor", "script", "git", "context",
    "notes", "state", "docs", "run", "gate", "code", "diagram", "graph", "kanban", "standup",
    "shell", "review", "todo", "race", "timeline", "design", "sketch", "fuel", "notebook",
    "radar", "ultraplan", "breakdown", "judgment", "swarm", "intent", "orchestrator",
];

// The PANE BRIEF block + its marker upsert live in `cockpit__helpers_pane_brief_md` since
// 2026-09-14 so `pane/context/bake` and `pane/update` rewrite the same block this fn writes first.
use crate::cockpit__helpers_pane_brief_md::upsert_brief;
#[cfg(test)]
use crate::cockpit__helpers_pane_brief_md::BRIEF_A;

/// Is a BYOK key on this box for the pane proxy's upstream? Same resolution as serve's `proxy_key`
/// (env, then `<home>/.antiloki/auth/<file>`, then `./.antiloki/auth/<file>`).
pub fn byok_key_present(env_key: &str, key_file: &str) -> bool {
    let home = std::env::var("ANTILOKI_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        let mut p = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).map(PathBuf::from).unwrap_or_default(); p.push(".antiloki"); p
    });
    byok_key_present_in(&home, std::env::var(env_key).ok().as_deref(), key_file)
}
fn byok_key_present_in(home: &Path, env_val: Option<&str>, key_file: &str) -> bool {
    if let Some(v) = env_val { let v = v.trim(); if !v.is_empty() && v != "antiloki-pane-proxy" { return true; } }
    [home.join("auth").join(key_file), PathBuf::from(".antiloki").join("auth").join(key_file)]
        .iter().any(|c| std::fs::read_to_string(c).map(|k| !k.trim().is_empty()).unwrap_or(false))
}

/// The `opencode` kind is the user's OWN OpenCode login (a subscription engine, like claude-code and
/// codex) unless an OpenRouter key is on the box — then the pane proxy meters it (BYOK). Without the
/// key, forcing the proxy model made every call fail "no openrouter key on the substrate"
/// (2026-09-19, the operator logged into OpenCode Zen and the pane still could not answer).
pub fn opencode_uses_proxy(pane_kind: &str, model_id: Option<&str>) -> bool {
    match pane_kind {
        "opencode-direct" => true,
        "opencode" => model_id.is_some() || byok_key_present("OPENROUTER_API_KEY", "openrouter.key"),
        _ => false,
    }
}

/// THE OPENCODE GO/ZEN CATALOG BY TIER (operator 2026-09-19: "subscribed to opencode go … deepseek v4
/// flash"). Probed for real with `opencode run -m`: deepseek-v4-flash/pro and glm-5.3 answer on this
/// plan, kimi-k2.7-code is NOT_FOUND, and the TUI's own default (a GPT model) 404s — so a pane and a
/// headless run both pin one. `ANTILOKI_TIER_MODEL_OPENCODE_<LOW|MID|HIGH>` overrides a cell
/// (empty or "default" = let OpenCode choose).
pub fn opencode_go_model(tier: &str) -> Option<String> {
    let t = tier.trim().to_ascii_lowercase();
    if let Ok(v) = std::env::var(format!("ANTILOKI_TIER_MODEL_OPENCODE_{}", t.to_ascii_uppercase())) { let v = v.trim().to_string(); return if v.is_empty() || v == "default" { None } else { Some(v) }; }
    // NO MODEL OF OUR OWN (operator 2026-11-26: "if opencode works on my terminal it should already work on antiloki"). A pinned paid model failed with "out of credits" where the terminal answered:
    // OpenCode now starts on the person's own default, and only an explicit ANTILOKI_TIER_MODEL_OPENCODE_* names one.
    None
}

/// opencode.json WITHOUT a provider: the user's own login, the mid-tier Go model pinned.
fn opencode_json_own_login() -> String {
    let mut cfg = Map::new();
    cfg.insert("$schema".into(), json!("https://opencode.ai/config.json"));
    if let Some(m) = opencode_go_model("mid") { cfg.insert("model".into(), json!(m)); }
    let mut perm = Map::new();
    perm.insert("edit".into(), json!("allow"));
    perm.insert("bash".into(), json!("ask"));
    cfg.insert("permission".into(), Value::Object(perm));
    serde_json::to_string_pretty(&Value::Object(cfg)).unwrap_or_default()
}

fn opencode_json(pane_id: i64, model: &str, port: &str) -> String {
    let mut options = Map::new();
    options.insert("baseURL".into(), json!(format!("http://127.0.0.1:{port}/api/v1/ai-proxy/pane/{pane_id}")));
    options.insert("apiKey".into(), json!("antiloki-pane-proxy"));
    let mut model_entry = Map::new();
    model_entry.insert("name".into(), json!(model));
    let mut models = Map::new();
    models.insert(model.to_string(), Value::Object(model_entry));
    let mut provider_entry = Map::new();
    provider_entry.insert("npm".into(), json!("@ai-sdk/openai-compatible"));
    provider_entry.insert("name".into(), json!("antiloki pane proxy"));
    provider_entry.insert("options".into(), Value::Object(options));
    provider_entry.insert("models".into(), Value::Object(models));
    let mut provider = Map::new();
    provider.insert("antiloki-proxy".into(), Value::Object(provider_entry));
    let mut cfg = Map::new();
    cfg.insert("$schema".into(), json!("https://opencode.ai/config.json"));
    cfg.insert("provider".into(), Value::Object(provider));
    cfg.insert("model".into(), json!(format!("antiloki-proxy/{model}")));
    let mut perm = Map::new();
    perm.insert("edit".into(), json!("allow"));
    perm.insert("bash".into(), json!("ask"));
    cfg.insert("permission".into(), Value::Object(perm));
    serde_json::to_string_pretty(&Value::Object(cfg)).unwrap_or_default()
}

/// The best-effort SYNCHRONOUS half of the "build the environment on creation" block — see the
/// module doc's PARITY-DEFERRED note for what is intentionally NOT ported (the streamed
/// stdout/stderr + close-handler completion note, which need a `'static` db handle this
/// synchronous signature does not have).
fn maybe_bootstrap_env(db: &Connection, session_id: i64, pane_id: i64, worktree_path: &Path) {
    let has_pkg = worktree_path.join("package.json").exists();
    let has_modules = worktree_path.join("node_modules").exists();
    if !has_pkg || has_modules { return; }
    let has_lock = worktree_path.join("package-lock.json").exists();
    let install_cmd = if has_lock { "npm ci --no-audit --no-fund" } else { "npm install --no-audit --no-fund" };
    let live_key = format!("vibe-pane-{pane_id}");
    let _ = crate::db_json::run_write(
        db,
        "INSERT INTO cockpit_blackboard (session_id, pane_id, kind, body_md) VALUES (?,?,?,?)",
        &[json!(session_id), json!(pane_id), json!("note"), json!(format!("⏳ building the environment — {install_cmd} running in the worktree…"))],
    );
    let _ = crate::db_json::run_write(
        db,
        "INSERT INTO build_gate_live (contract_slug, phase, seq, chunk, done) VALUES (?, 'install', ?, ?, ?)",
        &[json!(live_key), json!(0i64), json!(format!("$ {install_cmd}   (environment bootstrap on worktree creation)\n")), json!(0i64)],
    );
    // Real fire-and-forget side effect (mirrors JS's spawn(...).unref()): detached, output discarded.
    let _ = crate::proc::cmd("cmd")
        .args(["/C", install_cmd])
        .current_dir(worktree_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[allow(non_snake_case)]
/// What is at the pane's worktree path BEFORE we ask git for it.
///
/// `~/.antiloki/worktrees/` is one directory for every workspace, but the name it gets —
/// `cockpit-<session>-<idx>` — comes from ids that are per-workspace. Two workspaces that each
/// opened one pane both want `cockpit-1-0`, and the second one finds the first one's checkout
/// sitting there (its `.git` file points into the OTHER repo). `worktreeAdd` rightly refuses to
/// clobber an existing path without `force`; the pane then lands with no worktree and a `warn`
/// nobody reads. This tells the two cases apart so the caller can reuse or recreate.
#[derive(Debug, PartialEq, Eq)]
enum WorktreeDir {
    Absent,
    /// `git worktree list` of THIS workspace names it — reuse as-is.
    Registered { branch: Option<String> },
    /// A directory exists but this repo does not know it: another workspace's pane, or a pane
    /// whose row was removed without cleanup. Safe to recreate.
    Stale,
}

fn worktree_dir_disposition(root: &Path, path: &Path) -> WorktreeDir {
    if !path.exists() {
        return WorktreeDir::Absent;
    }
    let want = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let out = crate::proc::cmd("git").args(["worktree", "list", "--porcelain"]).current_dir(root).output();
    let Ok(out) = out else { return WorktreeDir::Stale };
    // porcelain: blocks of `worktree <path>` / `HEAD <sha>` / `branch refs/heads/<name>` lines
    let text = String::from_utf8_lossy(&out.stdout);
    for block in text.split("\n\n") {
        let mut wt: Option<PathBuf> = None;
        let mut branch: Option<String> = None;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                wt = Some(std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p)));
            } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
                branch = Some(b.to_string());
            }
        }
        if wt.as_deref() == Some(want.as_path()) {
            return WorktreeDir::Registered { branch };
        }
    }
    WorktreeDir::Stale
}

pub fn cockpitPaneAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    // context.db is always injected by the harness/runtime; the JS `if (!db) throw` guard is unreachable here.
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    let session_id_f = js_number(input.get("session_id"));
    if !truthy(Some(&json!(session_id_f))) { return Err("cockpit-pane-add: session_id is required".into()); }
    let session_id = session_id_f as i64;

    let session_rows = crate::db_json::query_rows(db, "SELECT id, mode FROM cockpit_sessions WHERE id = ?", &[id_param(session_id_f)])?;
    let session = match session_rows.as_array().and_then(|a| a.first()) {
        Some(s) => s.clone(),
        None => {
            let mut out = Map::new();
            out.insert("ok".into(), json!(false));
            out.insert("error".into(), json!(format!("session {} not found", num_tmpl(session_id_f))));
            return Ok(Value::Object(out));
        }
    };
    let session_mode = session.get("mode").and_then(Value::as_str).unwrap_or("").to_string();

    // TWO DIFFERENT QUESTIONS, and conflating them made a session permanently full (2026-09-24). Found by
    // hitting it: "pane cap reached (20 panes in steer mode)" on a session whose twenty panes were nine live
    // ones and eleven closed. The CAP asks how many panes are OPEN — a closed one occupies no place in the
    // layout. The INDEX asks what is free, and must avoid every row that exists, open or not, because
    // `UNIQUE(session_id, idx)` does not care whether a pane is closed. Asking one query both things meant the
    // next index climbed past the cap and stayed there forever, with nothing open in the session.
    // A CLOSED PANE LEAVES THE LAYOUT (2026-09-24). `idx` is a place in the session's grid and `UNIQUE(session_id,
    // idx)` enforces one pane per place — so a closed pane kept holding its slot, the next index climbed past the
    // cap, and the session was full forever. Found by hitting it: "pane cap reached (20 panes in steer mode)" on
    // a session of nine live panes and eleven closed. Parking the closed ones at a negative index frees the slot
    // WITHOUT deleting the row, which is the record of work that actually happened; negatives can never collide
    // with a real position and read plainly as "not on the grid".
    let _ = crate::db_json::run_write(db, "UPDATE cockpit_panes SET idx = -id WHERE session_id = ? AND COALESCE(status, '') = 'closed' AND idx >= 0", &[id_param(session_id_f)]);
    let used_rows = crate::db_json::query_rows(db, "SELECT idx FROM cockpit_panes WHERE session_id = ?", &[id_param(session_id_f)])?;
    let used: Vec<i64> = used_rows.as_array().map(|a| a.iter().filter_map(|r| r.get("idx").and_then(Value::as_i64)).collect()).unwrap_or_default();

    // RE-PROVISION: a pane whose worktree never landed (row present, worktree_path NULL) is
    // picked up again by the same add call rather than duplicated — the FE reopens a pane by
    // its subagent_name, and "already provisioned" must not be the answer for a pane with no
    // box. Everything below runs as if the row were new, minus the INSERT.
    let pending: Option<(i64, i64)> = match input.get("subagent_name").and_then(Value::as_str) {
        Some(name) if !name.is_empty() => {
            let rows = crate::db_json::query_rows(
                db,
                "SELECT id, idx FROM cockpit_panes WHERE session_id = ? AND subagent_name = ? AND worktree_path IS NULL ORDER BY id LIMIT 1",
                &[id_param(session_id_f), json!(take_chars(name, 80))],
            )?;
            rows.as_array().and_then(|a| a.first()).and_then(|r| Some((r.get("id")?.as_i64()?, r.get("idx")?.as_i64()?)))
        }
        _ => None,
    };

    let cap: i64 = if session_mode == "monitor" { 24 } else { 20 };
    let idx_input = input.get("idx").filter(|v| !v.is_null());
    let idx: i64 = match (pending, idx_input) {
        (Some((_, pidx)), _) => pidx,
        (None, Some(v)) => js_number(Some(v)) as i64,
        (None, None) => { let mut i = 0i64; while used.contains(&i) { i += 1; } i }
    };
    let open_now: i64 = crate::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM cockpit_panes WHERE session_id = ? AND COALESCE(status, '') != 'closed'", &[id_param(session_id_f)])?
        .as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).and_then(Value::as_i64).unwrap_or(0);
    // a re-provision of a row that already exists is not a new pane and does not spend a slot
    if pending.is_none() && open_now >= cap {
        let mut out = Map::new();
        out.insert("ok".into(), json!(false));
        out.insert("error".into(), json!(format!("pane cap reached ({cap} panes in {session_mode} mode)")));
        return Ok(Value::Object(out));
    }
    if idx >= cap {
        let mut out = Map::new();
        out.insert("ok".into(), json!(false));
        out.insert("error".into(), json!(format!("pane cap reached ({cap} panes in {session_mode} mode)")));
        return Ok(Value::Object(out));
    }
    if pending.is_none() && used.contains(&idx) {
        let mut out = Map::new();
        out.insert("ok".into(), json!(false));
        out.insert("error".into(), json!(format!("pane idx {idx} already in use")));
        return Ok(Value::Object(out));
    }

    let base_agent: Option<String> = {
        let s = take_chars(&str_qq(input.get("base_agent")), 80);
        if s.is_empty() { None } else { Some(s) }
    };
    let subagent_name_v = input.get("subagent_name").filter(|v| !v.is_null());
    let subagent_raw = if let Some(v) = subagent_name_v {
        str_qq(Some(v))
    } else if let Some(ba) = &base_agent {
        ba.clone()
    } else {
        format!("pane-{idx}")
    };
    let subagent = take_chars(&subagent_raw, 80);

    let scope: Vec<String> = input.get("scope").and_then(Value::as_array).map(|a| a.iter().map(js_string).collect()).unwrap_or_default();
    let grants: Vec<String> = match input.get("grants").and_then(Value::as_array) {
        Some(a) => a.iter().map(js_string).collect(),
        None => role_grants(base_agent.as_deref()).into_iter().map(String::from).collect(),
    };
    let tier: String = match input.get("tier").and_then(Value::as_str) {
        Some(t @ ("low" | "mid" | "high")) => t.to_string(),
        _ => "low".to_string(),
    };
    let extra_prompt: Option<String> = if truthy(input.get("extra_prompt_md")) { Some(take_chars(&str_qq(input.get("extra_prompt_md")), 8000)) } else { None };
    let model_id: Option<String> = if truthy(input.get("model_id")) { Some(take_chars(&str_qq(input.get("model_id")), 200)) } else { None };
    let tag = crate::agents_git_manager_git_manager_worktree_ops::project_tag(crate::engine_workspace_root::getWorkspaceRoot(db).as_str().unwrap_or_default());
    let card_slug = format!("{}{}cockpit-{}-{idx}", tag, if tag.is_empty() { "" } else { "-" }, num_tmpl(session_id_f));
    let branch = card_slug.clone();
    let pane_kind: String = match input.get("pane_kind").and_then(Value::as_str) {
        // `x-<provider>` is an agent provider the person added (serve/src/providers.rs)
        Some(k) if PANE_KINDS.contains(&k) || (k.len() > 3 && k.len() <= 34 && k.starts_with("x-") && k.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')) => k.to_string(),
        _ => "agent".to_string(),
    };

    // OVERLAP IS INFORMATION, NOT A REFUSAL (operator 2026-09-20: "every time I spawn a new agent a new
    // worktree is made, nothing overlaps nothing"): each pane edits its own box; two boxes claiming the
    // same files only meet at the merge, and git says so there. The overlaps are reported so the
    // operator knows which panes may collide — the pane is added regardless.
    let overlaps: Vec<Value> = if scope.is_empty() { vec![] } else {
        let existing = crate::db_json::query_rows(db, "SELECT pane_id, path_glob FROM cockpit_claims WHERE session_id = ?", &[id_param(session_id_f)])?;
        let existing = existing.as_array().cloned().unwrap_or_default();
        let mut out = vec![];
        for g in &scope {
            let g_v = json!(g);
            for e in existing.iter().filter(|e| crate::cockpit__helpers_scope::globsOverlap(&g_v, e.get("path_glob").unwrap_or(&Value::Null)).as_bool().unwrap_or(false)) {
                out.push(json!({ "scope": g, "pane_id": e.get("pane_id").cloned().unwrap_or(Value::Null), "glob": e.get("path_glob").cloned().unwrap_or(Value::Null) }));
            }
        }
        out
    };

    let settings_json: Value = match input.get("settings") {
        Some(v @ (Value::Object(_) | Value::Array(_))) => json!(take_chars(&serde_json::to_string(v).unwrap_or_default(), 4000)),
        _ => Value::Null,
    };

    let pane_id: i64 = match pending {
        Some((pid, _)) => pid,
        None => {
            let (_, pid) = crate::db_json::run_write(
                db,
                "INSERT INTO cockpit_panes (session_id, idx, subagent_name, scope_json, grants_json, tier, base_agent, extra_prompt_md, model_id, branch, pane_kind, settings_json, status, last_activity)
                 VALUES (?,?,?,?,?,?,?,?,?,?,?,?, 'idle', datetime('2026-01-01 00:00:00'))",
                &[
                    id_param(session_id_f),
                    json!(idx),
                    json!(subagent),
                    json!(serde_json::to_string(&scope).unwrap_or_default()),
                    json!(serde_json::to_string(&grants).unwrap_or_default()),
                    json!(tier),
                    base_agent.as_ref().map(|s| json!(s)).unwrap_or(Value::Null),
                    extra_prompt.as_ref().map(|s| json!(s)).unwrap_or(Value::Null),
                    model_id.as_ref().map(|s| json!(s)).unwrap_or(Value::Null),
                    json!(branch),
                    json!(pane_kind),
                    settings_json,
                ],
            )?;
            for g in &scope {
                crate::db_json::run_write(db, "INSERT INTO cockpit_claims (session_id, pane_id, path_glob) VALUES (?,?,?)", &[id_param(session_id_f), json!(pid), json!(g)])?;
            }
            pid
        }
    };

    let mut worktree: Value = Value::Null;
    let mut warn: Option<String> = None;
    let mut context_pack: Value = Value::Null;

    let provision = !matches!(input.get("provision"), Some(Value::Bool(false)));
    if provision {
        let root = crate::engine_workspace_root::getWorkspaceRoot(db).as_str().unwrap_or_default().to_string();
        // A FREED SLOT IS NOT A USED BOX (2026-11-11): a closed pane's place in the grid is handed out again (above), and its folder is `cockpit-<session>-<idx>`, so the NEXT agent in that place found the old agent's registered checkout and was
        // "reattached" to it: the old conversation, episodes, prompts and changes, as if it were the new agent's own. Closing agents on the screen now releases their rows, which makes this the ordinary case. If a pane row (open or closed) already
        // owns that folder, the new pane gets a folder of its own.
        let card_slug = {
            let planned = crate::agents_git_manager_git_manager_worktree_ops::worktreePathFor(&json!(card_slug)).to_string_lossy().into_owned();
            let owned = crate::db_json::query_rows(db, "SELECT 1 FROM cockpit_panes WHERE worktree_path = ? AND id != ? LIMIT 1", &[json!(planned), json!(pane_id)]).map(|r| r.as_array().map_or(false, |a| !a.is_empty())).unwrap_or(false);
            if owned { format!("{card_slug}-{}", format!("{:x}", crate::clock::now_ms())) } else { card_slug }
        };
        let wt_path_planned = crate::agents_git_manager_git_manager_worktree_ops::worktreePathFor(&json!(card_slug));
        let wt_result: Result<Value, String> = match worktree_dir_disposition(Path::new(&root), &wt_path_planned) {
            // this workspace's own checkout is already there — a re-provision, or a pane row lost
            // while its box survived. Reattach rather than wipe.
            WorktreeDir::Registered { branch: b } => Ok(json!({
                "card_slug": card_slug,
                "path": wt_path_planned.to_string_lossy().into_owned(),
                "branch": b.map(|x| json!(x)).unwrap_or(Value::Null),
                "created": false,
            })),
            // another workspace's `cockpit-<same numbers>` — not ours to keep; recreate.
            WorktreeDir::Stale => crate::agents_git_manager_git_manager_worktree_ops::worktreeAdd(&json!({ "card_slug": card_slug, "force": true }), db),
            WorktreeDir::Absent => crate::agents_git_manager_git_manager_worktree_ops::worktreeAdd(&json!({ "card_slug": card_slug, "from": input.get("from").cloned().unwrap_or(Value::Null) }), db),
        };
        match wt_result {
            Ok(wt) => {
                let wt_path_str = wt.get("path").and_then(Value::as_str).unwrap_or("").to_string();
                worktree = json!(wt_path_str.clone());
                let wt_branch = if truthy(wt.get("branch")) { str_qq(wt.get("branch")) } else { branch.clone() };
                let _ = crate::db_json::run_write(db, "UPDATE cockpit_panes SET worktree_path = ?, branch = ? WHERE id = ?", &[json!(wt_path_str.clone()), json!(wt_branch), json!(pane_id)]);

                let proxied = pane_kind == "opencode" || pane_kind == "opencode-direct";
                if matches!(pane_kind.as_str(), "opencode" | "opencode-direct" | "claude-code" | "codex") {
                    let wt_path = PathBuf::from(&wt_path_str);
                    let engine_result: Result<(), String> = (|| {
                        if !scope.is_empty() && std::env::var("ANTILOKI_VIBE_SPARSE").as_deref() == Ok("1") {
                            let mut args: Vec<&str> = vec!["sparse-checkout", "set", "--no-cone"];
                            for g in &scope { args.push(g.as_str()); }
                            args.push("/package.json");
                            args.push("/package-lock.json");
                            args.push("/AGENTS.md");
                            if proxied { args.push("/opencode.json"); }
                            if pane_kind == "claude-code" { args.push("/CLAUDE.md"); }
                            match crate::proc::cmd("git").args(&args).current_dir(&wt_path).output() {
                                Ok(o) if o.status.success() => {}
                                Ok(o) => { warn = Some(format!("sparse-checkout failed (full checkout kept): {}", take_chars(&String::from_utf8_lossy(&o.stderr), 120))); }
                                Err(e) => { warn = Some(format!("sparse-checkout failed (full checkout kept): {}", take_chars(&e.to_string(), 120))); }
                            }
                        }
                        if proxied {
                            let port = std::env::var("ANTILOKI_BE_PORT").or_else(|_| std::env::var("PORT")).unwrap_or_else(|_| "41931".to_string());
                            let cfg_str = if opencode_uses_proxy(&pane_kind, model_id.as_deref()) {
                                let model = model_id.clone().unwrap_or_else(|| if pane_kind == "opencode-direct" { "gpt-4o".to_string() } else { "openrouter/auto".to_string() });
                                opencode_json(pane_id, &model, &port)
                            } else { opencode_json_own_login() };
                            std::fs::write(wt_path.join("opencode.json"), cfg_str).map_err(|e| e.to_string())?;
                        }
                        let docs = crate::db_json::query_rows(db, "SELECT id, body_md FROM cockpit_blackboard WHERE session_id = ? AND kind = 'context'", &[id_param(session_id_f)])?;
                        let docs = docs.as_array().cloned().unwrap_or_default();
                        // the first brief carries the docs inline; the context bake below replaces it
                        // with the pack's paths as soon as the pack exists.
                        let brief = crate::cockpit__helpers_pane_brief_md::brief_md(&crate::cockpit__helpers_pane_brief_md::BriefInput {
                            extra_prompt_md: extra_prompt.as_deref(),
                            scope: &scope,
                            manifest: None,
                            docs: &docs,
                            live_tools: false,
                            antiqa: false,
                            intro: None,   // the bake that follows writes the catalog's introduction
                        });
                        upsert_brief(&wt_path.join("AGENTS.md"), &brief);
                        if pane_kind == "claude-code" { upsert_brief(&wt_path.join("CLAUDE.md"), &brief); }
                        Ok(())
                    })();
                    if let Err(e2) = engine_result {
                        if warn.is_none() { warn = Some(format!("engine provision partial: {}", take_chars(&e2, 120))); }
                    }
                }

                maybe_bootstrap_env(db, session_id, pane_id, &PathBuf::from(&wt_path_str));

                // CONTEXT PACK: the measured view of the pane's scope, on disk before the CLI boots.
                // Best-effort like everything else here — a failed bake leaves the brief above intact.
                if matches!(pane_kind.as_str(), "opencode" | "opencode-direct" | "claude-code" | "codex" | "agent") && !matches!(input.get("context_bake"), Some(Value::Bool(false))) {
                    match crate::cockpit_cockpit_pane_context_bake::cockpitPaneContextBake(&json!({ "pane_id": pane_id }), db) {
                        Ok(v) if v.get("ok") == Some(&json!(true)) => { context_pack = v; }
                        Ok(v) => { if warn.is_none() { warn = Some(format!("context pack not baked: {}", take_chars(&str_qq(v.get("error")), 120))); } }
                        Err(e) => { if warn.is_none() { warn = Some(format!("context pack not baked: {}", take_chars(&e, 120))); } }
                    }
                }
            }
            Err(e) => {
                warn = Some(format!("worktree pending: {}", take_chars(&e, 140)));
            }
        }
    }

    let row_rows = crate::db_json::query_rows(db, "SELECT * FROM cockpit_panes WHERE id = ?", &[json!(pane_id)])?;
    let mut row = row_rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null).as_object().cloned().unwrap_or_default();
    row.insert("scope".into(), json!(scope));
    row.insert("grants".into(), json!(grants));

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("pane".into(), Value::Object(row));
    out.insert("worktree".into(), worktree);
    out.insert("warn".into(), warn.map(|w| json!(w)).unwrap_or(Value::Null));
    out.insert("overlaps".into(), json!(overlaps));
    out.insert("context_pack".into(), context_pack);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn with_home<T>(antiloki_home: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ANTILOKI_HOME", antiloki_home); }
        let result = f();
        unsafe { std::env::remove_var("ANTILOKI_HOME"); }
        result
    }

    /// THE TESTS THAT WERE NOT ISOLATED (2026-10-07, CI run 37597199148 failed on a sibling of an already-listed flake; 18 of 40 local runs of this module failed, a DIFFERENT
    /// test each time): ten tests called `cockpitPaneAdd` without `with_home`, so they read `ANTILOKI_HOME` while another test was setting it - landing in that test's worktrees
    /// directory (the same `cockpit-1-0` slugs) or in the default home. This guard holds the shared `ENV_LOCK` for the whole test and points `ANTILOKI_HOME` at a private temp dir.
    /// Never take it inside a `with_home` (the lock is not re-entrant).
    struct HomeGuard { _lock: std::sync::MutexGuard<'static, ()>, home: PathBuf }
    fn isolated_home(tag: &str) -> HomeGuard {
        let lock = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let home = temp_dir(&format!("{tag}-home"));
        unsafe { std::env::set_var("ANTILOKI_HOME", &home); }
        HomeGuard { _lock: lock, home }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) { unsafe { std::env::remove_var("ANTILOKI_HOME"); } let _ = fs::remove_dir_all(&self.home); }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-cockpit-pane-add-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run_git(args: &[&str], cwd: &Path) {
        // spawning git resolves it through PATH, which other tests empty under ENV_LOCK (lint-ruff): without the lock this
        // test's fixture setup failed at random ("git not found") when the two ran at the same moment
        let _g = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let out = crate::proc::cmd("git").args(args).current_dir(cwd).output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }

    fn make_temp_git_repo(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        run_git(&["init", "-q"], &dir);
        run_git(&["config", "user.email", "test@test"], &dir);
        run_git(&["config", "user.name", "test"], &dir);
        fs::write(dir.join("README.md"), "# test\n").unwrap();
        run_git(&["add", "README.md"], &dir);
        run_git(&["commit", "-q", "-m", "init"], &dir);
        dir
    }

    fn db_with_workspace(root: &Path) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');
             CREATE TABLE build_gate_live (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, phase TEXT, seq INTEGER, chunk TEXT, done INTEGER);",
        )
        .unwrap();
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', ?1, 1)", rusqlite::params![root.to_string_lossy().into_owned()]).unwrap();
        c
    }

    fn insert_session(c: &Connection, id: i64, mode: &str) {
        c.execute("INSERT INTO cockpit_sessions (id, name, status, mode) VALUES (?,?,?,?)", rusqlite::params![id, "s", "active", mode]).unwrap();
    }

    #[test]
    fn session_id_required() {
        let _home = isolated_home("session_id_required");
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        let e = cockpitPaneAdd(&json!({}), &c).unwrap_err();
        assert_eq!(e, "cockpit-pane-add: session_id is required");
    }

    #[test]
    fn session_not_found_returns_ok_false() {
        let _home = isolated_home("session_not_found_return");
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        let r = cockpitPaneAdd(&json!({ "session_id": 42, "provision": false }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "session 42 not found" }));
    }

    #[test]
    fn cap_reached_and_idx_conflict() {
        let _home = isolated_home("cap_reached_and_idx_conf");
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        insert_session(&c, 1, "steer");
        let r = cockpitPaneAdd(&json!({ "session_id": 1, "idx": 20, "provision": false }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "pane cap reached (20 panes in steer mode)" }));

        let r2 = cockpitPaneAdd(&json!({ "session_id": 1, "idx": 0, "provision": false }), &c).unwrap();
        assert_eq!(r2["ok"], json!(true));
        let r3 = cockpitPaneAdd(&json!({ "session_id": 1, "idx": 0, "provision": false }), &c).unwrap();
        assert_eq!(r3, json!({ "ok": false, "error": "pane idx 0 already in use" }));
    }

    // FOUND BY HITTING IT (2026-09-24): a session of twenty panes, nine live and eleven closed, refused a new
    // agent with "pane cap reached". The count had no status filter, so every pane ever made held its slot for
    // ever and the session was permanently full with nothing open in it.
    #[test]
    fn a_closed_pane_gives_its_slot_back() {
        let _home = isolated_home("a_closed_pane_gives_its_");
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        insert_session(&c, 1, "steer");
        for i in 0..20 { assert_eq!(cockpitPaneAdd(&json!({ "session_id": 1, "idx": i, "provision": false }), &c).unwrap()["ok"], json!(true)); }
        // full, correctly: twenty panes are open
        assert_eq!(cockpitPaneAdd(&json!({ "session_id": 1, "provision": false }), &c).unwrap()["ok"], json!(false));

        c.execute("UPDATE cockpit_panes SET status = 'closed' WHERE idx IN (3, 7)", []).unwrap();
        let r = cockpitPaneAdd(&json!({ "session_id": 1, "provision": false }), &c).unwrap();
        assert_eq!(r["ok"], json!(true), "eighteen panes are open, so there is room: {r}");
        assert_eq!(r["pane"]["idx"], json!(3), "and it takes the lowest slot a closed pane gave back");

        // the closed rows are still there — they are the record of work that happened — parked off the grid
        let rows = crate::db_json::query_rows(&c, "SELECT COUNT(*) AS n FROM cockpit_panes WHERE session_id = 1", &[]).unwrap();
        assert_eq!(rows[0]["n"], json!(21), "nothing was deleted to make room");
        let parked = crate::db_json::query_rows(&c, "SELECT COUNT(*) AS n FROM cockpit_panes WHERE session_id = 1 AND idx < 0", &[]).unwrap();
        assert_eq!(parked[0]["n"], json!(2), "both closed panes left the layout rather than being deleted");

        // and with twenty open again, it is full — correctly this time
        assert_eq!(cockpitPaneAdd(&json!({ "session_id": 1, "provision": false }), &c).unwrap()["pane"]["idx"], json!(7));
        assert_eq!(cockpitPaneAdd(&json!({ "session_id": 1, "provision": false }), &c).unwrap()["ok"], json!(false));
    }

    #[test]
    fn monitor_mode_raises_cap_to_24() {
        let _home = isolated_home("monitor_mode_raises_cap_");
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        insert_session(&c, 1, "monitor");
        let r = cockpitPaneAdd(&json!({ "session_id": 1, "idx": 20, "provision": false }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let r2 = cockpitPaneAdd(&json!({ "session_id": 1, "idx": 24, "provision": false }), &c).unwrap();
        assert_eq!(r2, json!({ "ok": false, "error": "pane cap reached (24 panes in monitor mode)" }));
    }

    #[test]
    fn idx_auto_assigns_first_free_slot() {
        let _home = isolated_home("idx_auto_assigns_first_f");
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        insert_session(&c, 1, "steer");
        let r0 = cockpitPaneAdd(&json!({ "session_id": 1, "provision": false }), &c).unwrap();
        assert_eq!(r0["pane"]["idx"], json!(0));
        let r1 = cockpitPaneAdd(&json!({ "session_id": 1, "provision": false }), &c).unwrap();
        assert_eq!(r1["pane"]["idx"], json!(1));
    }

    #[test]
    fn defaults_role_grants_and_pane_kind() {
        let _home = isolated_home("defaults_role_grants_and");
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        insert_session(&c, 1, "steer");
        let r = cockpitPaneAdd(&json!({ "session_id": 1, "base_agent": "qa", "provision": false }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["pane"]["grants"], json!(["read", "run"]));
        assert_eq!(r["pane"]["pane_kind"], json!("agent"));
        assert_eq!(r["pane"]["subagent_name"], json!("qa")); // subagent_name falls back to base_agent
        assert_eq!(r["worktree"], Value::Null);
        assert_eq!(r["warn"], Value::Null);
    }

    #[test]
    fn scope_overlap_is_reported_never_refused() {
        let _home = isolated_home("scope_overlap_is_reporte");
        // operator 2026-09-20: every pane has its own worktree — an overlap only matters at the merge
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        insert_session(&c, 1, "steer");
        c.execute("INSERT INTO cockpit_claims (session_id, pane_id, path_glob) VALUES (1, 9, 'src/**')", []).unwrap();
        let r = cockpitPaneAdd(&json!({ "session_id": 1, "scope": ["src/ui/**"], "provision": false }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["overlaps"], json!([{ "scope": "src/ui/**", "pane_id": 9, "glob": "src/**" }]));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM cockpit_panes", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
        let claims: i64 = c.query_row("SELECT COUNT(*) FROM cockpit_claims WHERE path_glob = 'src/ui/**'", [], |row| row.get(0)).unwrap();
        assert_eq!(claims, 1, "the new pane's claim is registered beside the old one");
    }

    #[test]
    fn scope_registers_claims_on_success() {
        let _home = isolated_home("scope_registers_claims_o");
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        insert_session(&c, 1, "steer");
        let r = cockpitPaneAdd(&json!({ "session_id": 1, "scope": ["src/ui/**", "docs/**"], "provision": false }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM cockpit_claims WHERE session_id = 1", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn worktree_failure_sets_warn_but_pane_still_created() {
        let _home = isolated_home("worktree_failure_sets_wa");
        let c = Connection::open_in_memory().unwrap(); // no `workspaces` row -> getWorkspaceRoot/git fails
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c.execute_batch("CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, created_at TEXT)").unwrap();
        insert_session(&c, 1, "steer");
        let r = cockpitPaneAdd(&json!({ "session_id": 1 }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["worktree"], Value::Null);
        assert!(r["warn"].as_str().unwrap().starts_with("worktree pending:"));
    }

    #[test]
    fn worktree_provisioned_for_default_agent_kind_writes_no_brief() {
        let repo = make_temp_git_repo("pane-add-agent");
        let home = temp_dir("pane-add-agent-home");
        let c = db_with_workspace(&repo);
        insert_session(&c, 1, "steer");
        let r = with_home(home.to_str().unwrap(), || cockpitPaneAdd(&json!({ "session_id": 1 }), &c).unwrap());
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["warn"], Value::Null);
        let wt_path = PathBuf::from(r["worktree"].as_str().unwrap());
        assert!(wt_path.join("README.md").exists());
        assert!(!wt_path.join("AGENTS.md").exists()); // pane_kind 'agent' is not in the engine-provision list
        assert_eq!(r["pane"]["worktree_path"], json!(wt_path.to_string_lossy().into_owned()));
        assert!(r["pane"]["branch"].as_str().is_some_and(|b| b.ends_with("-cockpit-1-0")), "the folder says the project: {}", r["pane"]["branch"]);
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn another_workspaces_leftover_at_the_same_slug_is_recreated_not_refused() {
        // The operator's exact failure: workspace A opened pane cockpit-1-0 (session 1 / idx 0);
        // workspace B's first pane is ALSO session 1 / idx 0, finds A's checkout at the shared
        // ~/.antiloki/worktrees/cockpit-1-0, and used to land with worktree_path NULL.
        let repo_a = make_temp_git_repo("pane-add-stale-a");
        let repo_b = make_temp_git_repo("pane-add-stale-b");
        fs::write(repo_b.join("ONLY_IN_B.md"), "b\n").unwrap();
        run_git(&["add", "ONLY_IN_B.md"], &repo_b);
        run_git(&["commit", "-q", "-m", "b"], &repo_b);
        let home = temp_dir("pane-add-stale-home");
        let c_a = db_with_workspace(&repo_a);
        insert_session(&c_a, 1, "steer");
        let c_b = db_with_workspace(&repo_b);
        insert_session(&c_b, 1, "steer");
        let (ra, rb) = with_home(home.to_str().unwrap(), || {
            let ra = cockpitPaneAdd(&json!({ "session_id": 1, "subagent_name": "claude" }), &c_a).unwrap();
            let rb = cockpitPaneAdd(&json!({ "session_id": 1, "subagent_name": "claude" }), &c_b).unwrap();
            (ra, rb)
        });
        assert_eq!(ra["warn"], Value::Null);
        assert_eq!(rb["warn"], Value::Null, "B must not inherit A's leftover as a failure");
        assert_eq!(ra["worktree"], rb["worktree"], "same slug, same shared path");
        let wt = PathBuf::from(rb["worktree"].as_str().unwrap());
        assert!(wt.join("ONLY_IN_B.md").exists(), "the checkout at the slug is now B's, not A's");
        assert_eq!(rb["pane"]["worktree_path"], rb["worktree"]);
        let _ = fs::remove_dir_all(&repo_a);
        let _ = fs::remove_dir_all(&repo_b);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn reopening_a_pending_pane_reprovisions_the_same_row() {
        // First add fails to get a worktree (no workspace row → git has no root); the row lands
        // with worktree_path NULL. The same add, once the workspace is fixed, must fill THAT row
        // in rather than create a second pane with the same name.
        let repo = make_temp_git_repo("pane-add-reprov");
        let home = temp_dir("pane-add-reprov-home");
        let c = db_with_workspace(&repo);
        insert_session(&c, 1, "steer");
        c.execute("UPDATE workspaces SET root_path = '/nonexistent/nowhere'", []).unwrap();
        let first = with_home(home.to_str().unwrap(), || cockpitPaneAdd(&json!({ "session_id": 1, "subagent_name": "claude", "pane_kind": "claude-code" }), &c).unwrap());
        assert!(first["warn"].as_str().unwrap().starts_with("worktree pending"), "{}", first["warn"]);
        assert_eq!(first["pane"]["worktree_path"], Value::Null);
        c.execute("UPDATE workspaces SET root_path = ?1", rusqlite::params![repo.to_string_lossy().into_owned()]).unwrap();
        let second = with_home(home.to_str().unwrap(), || cockpitPaneAdd(&json!({ "session_id": 1, "subagent_name": "claude", "pane_kind": "claude-code" }), &c).unwrap());
        assert_eq!(second["warn"], Value::Null, "{}", second["warn"]);
        assert_eq!(second["pane"]["id"], first["pane"]["id"], "same row, re-provisioned");
        assert!(second["pane"]["worktree_path"].as_str().map_or(false, |p| Path::new(p).join("CLAUDE.md").exists()));
        let n: i64 = c.query_row("SELECT count(*) FROM cockpit_panes WHERE session_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "no duplicate pane row");
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn this_workspaces_own_registered_worktree_is_reused_not_wiped() {
        let repo = make_temp_git_repo("pane-add-reuse");
        let home = temp_dir("pane-add-reuse-home");
        let c = db_with_workspace(&repo);
        insert_session(&c, 1, "steer");
        let first = with_home(home.to_str().unwrap(), || cockpitPaneAdd(&json!({ "session_id": 1, "subagent_name": "claude" }), &c).unwrap());
        let wt = PathBuf::from(first["worktree"].as_str().unwrap());
        fs::write(wt.join("WIP.txt"), "uncommitted work\n").unwrap();
        // simulate a lost row (pane removed without cleanup) so the next add wants the same slug
        c.execute("DELETE FROM cockpit_panes", []).unwrap();
        let second = with_home(home.to_str().unwrap(), || cockpitPaneAdd(&json!({ "session_id": 1, "subagent_name": "claude" }), &c).unwrap());
        assert_eq!(second["warn"], Value::Null, "{}", second["warn"]);
        assert_eq!(second["worktree"], first["worktree"]);
        assert!(wt.join("WIP.txt").exists(), "a registered worktree of this repo is reattached, never recreated");
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn claude_code_pane_writes_brief_into_both_agents_and_claude_md_preserving_existing_content() {
        let repo = make_temp_git_repo("pane-add-cc");
        fs::write(repo.join("AGENTS.md"), "# existing repo docs\nkeep me\n").unwrap();
        run_git(&["add", "AGENTS.md"], &repo);
        run_git(&["commit", "-q", "-m", "add agents md"], &repo);
        let home = temp_dir("pane-add-cc-home");
        let c = db_with_workspace(&repo);
        insert_session(&c, 1, "steer");
        let r = with_home(home.to_str().unwrap(), || {
            cockpitPaneAdd(&json!({ "session_id": 1, "pane_kind": "claude-code", "extra_prompt_md": "focus on tests", "scope": ["src/ui/**"] }), &c).unwrap()
        });
        assert_eq!(r["ok"], json!(true));
        let wt_path = PathBuf::from(r["worktree"].as_str().unwrap());
        let agents_md = fs::read_to_string(wt_path.join("AGENTS.md")).unwrap();
        assert!(agents_md.contains(BRIEF_A));
        assert!(agents_md.contains("OPERATOR GUIDANCE:\nfocus on tests"));
        assert!(agents_md.contains("SCOPE (edit whitelist)"));
        assert!(agents_md.contains("keep me")); // original repo content preserved below the block
        let claude_md = fs::read_to_string(wt_path.join("CLAUDE.md")).unwrap();
        assert!(claude_md.contains(BRIEF_A));
        assert!(!wt_path.join("opencode.json").exists()); // claude-code is not `proxied`
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn opencode_runs_on_its_own_login_unless_a_byok_key_or_a_model_pin_asks_for_the_proxy() {
        let dir = std::env::temp_dir().join(format!("antiloki-byok-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("auth")).unwrap();
        assert!(!byok_key_present_in(&dir, None, "openrouter.key"), "no key on the box → the user's own OpenCode login");
        assert!(!byok_key_present_in(&dir, Some("antiloki-pane-proxy"), "openrouter.key"), "the pane's own placeholder is not a key");
        assert!(byok_key_present_in(&dir, Some("sk-or-env"), "openrouter.key"));
        std::fs::write(dir.join("auth").join("openrouter.key"), "sk-or-real").unwrap();
        assert!(byok_key_present_in(&dir, None, "openrouter.key"), "a key on the box → metered BYOK through the proxy");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(opencode_uses_proxy("opencode", Some("openrouter/auto")), "an explicit model pin means the proxy");
        assert!(opencode_uses_proxy("opencode-direct", None), "the OpenAI-key kind is always proxied");
        assert!(!opencode_uses_proxy("claude-code", None));
        let own: Value = serde_json::from_str(&opencode_json_own_login()).unwrap();
        assert!(own.get("provider").is_none(), "no proxy provider — the login's catalog");
        assert!(own.get("model").is_none(), "no model of ours is pinned");
        assert_eq!(opencode_go_model("low"), None, "the person's own default model");
        assert_eq!(opencode_go_model("junk"), None);
        assert_eq!(own["permission"]["edit"], json!("allow"));
    }
    #[test]
    fn opencode_pane_writes_opencode_json_config() {
        let repo = make_temp_git_repo("pane-add-oc");
        let home = temp_dir("pane-add-oc-home");
        let c = db_with_workspace(&repo);
        insert_session(&c, 1, "steer");
        let r = with_home(home.to_str().unwrap(), || {
            cockpitPaneAdd(&json!({ "session_id": 1, "pane_kind": "opencode", "model_id": "gpt-4o-mini" }), &c).unwrap()
        });
        assert_eq!(r["ok"], json!(true));
        let wt_path = PathBuf::from(r["worktree"].as_str().unwrap());
        let cfg: Value = serde_json::from_str(&fs::read_to_string(wt_path.join("opencode.json")).unwrap()).unwrap();
        assert_eq!(cfg["model"], json!("antiloki-proxy/gpt-4o-mini"));
        assert_eq!(cfg["provider"]["antiloki-proxy"]["models"]["gpt-4o-mini"]["name"], json!("gpt-4o-mini"));
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn mission_is_always_none_set_because_session_select_omits_mission_md() {
        let repo = make_temp_git_repo("pane-add-mission");
        let home = temp_dir("pane-add-mission-home");
        let c = db_with_workspace(&repo);
        c.execute("INSERT INTO cockpit_sessions (id, name, status, mode, mission_md) VALUES (1,'s','active','steer','REAL MISSION TEXT')", []).unwrap();
        let r = with_home(home.to_str().unwrap(), || {
            cockpitPaneAdd(&json!({ "session_id": 1, "pane_kind": "claude-code" }), &c).unwrap()
        });
        let wt_path = PathBuf::from(r["worktree"].as_str().unwrap());
        let brief = fs::read_to_string(wt_path.join("AGENTS.md")).unwrap();
        assert!(brief.contains("(none set)"));
        assert!(!brief.contains("REAL MISSION TEXT"));
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }
}
