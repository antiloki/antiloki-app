//! AGENT RECIPES (operator 2026-09-20: "an add button at agents so we can create new agents. The recipe is
//! choose a base provider (claude, codex, opencode), a default theme, an icon, default scope, then mix
//! specialists and skills, and create"). A recipe is what a Studio agent tile carries: when it is added
//! to a thread the pane is born with the provider, the scope, the theme, the specialists and the skills
//! the recipe names. Its own table — the `agents` persona table is bound to permissions and stays as is.
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::dispatch::AppState;

/// EVERY ENGINE THE ROUTER KNOWS, asked of `cli::Engine` rather than listed here (2026-09-27: this was a hardcoded
/// `["claude-code", "codex", "opencode"]`, so a recipe naming Gemini, Grok or Cursor was rejected as an unknown
/// provider). A function, not a const: `[T; N]::map` is not const-stable.
fn providers() -> [&'static str; 6] { crate::cli::Engine::all().map(|e| e.key()) }

fn body_json(b: &Bytes) -> Value { serde_json::from_slice(b).unwrap_or(Value::Null) }
fn s(v: Option<&Value>) -> String { v.and_then(Value::as_str).unwrap_or("").to_string() }
fn strs(v: Option<&Value>) -> Vec<String> { v.and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default() }

/// `name` → the slug the tile is keyed by. Pure.
pub fn slugify(name: &str) -> String {
    let mut out = String::new(); let mut dash = false;
    for c in name.trim().to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() { out.push(c); dash = false; } else if !dash && !out.is_empty() { out.push('-'); dash = true; }
    }
    out.trim_end_matches('-').chars().take(40).collect()
}
/// The table and its columns. PUBLIC because the roster reads `role` and `archived`, and a serve that has not yet
/// served a recipe request would otherwise query columns that do not exist yet — the roster came back empty on a
/// fresh process until it called this first (2026-09-23).
pub fn ensure(db: &rusqlite::Connection) {
    let _ = db.execute_batch("CREATE TABLE IF NOT EXISTS agent_recipes (slug TEXT PRIMARY KEY, name TEXT NOT NULL, icon TEXT NOT NULL DEFAULT '🤖', provider TEXT NOT NULL, theme TEXT NOT NULL DEFAULT '', description TEXT NOT NULL DEFAULT '', scope_json TEXT NOT NULL DEFAULT '[]', specialists_json TEXT NOT NULL DEFAULT '[]', skills_json TEXT NOT NULL DEFAULT '[]', ask TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL, updated_at TEXT NOT NULL)");
    // AN AGENT CARRIES A ROLE (operator 2026-09-23: "an agent carries role, skills, scope, specialists, avatar and
    // its episodes" · "all agents should have a role"). The role is what a person hires — everything else on the row
    // is how it does the job. Rows made before this have none and are given one on read, from what they are for.
    let _ = db.execute("ALTER TABLE agent_recipes ADD COLUMN role TEXT NOT NULL DEFAULT ''", []);
    let _ = db.execute("ALTER TABLE agent_recipes ADD COLUMN objective TEXT NOT NULL DEFAULT ''", []);
    // ARCHIVED (operator 2026-09-23: "add a button on settings of an agent to archive that agent"). An agent you
    // are done with leaves the roster; it is not deleted, because its episodes, its runs and its money are the
    // record of work that actually happened. Setting it back to 0 brings the agent back with all of it.
    let _ = db.execute("ALTER TABLE agent_recipes ADD COLUMN archived INTEGER NOT NULL DEFAULT 0", []);
    // A RECIPE BELONGS TO ITS WORKSPACE (operator 2026-09-20: "I switched workspace and a custom agent from the
    // previous one appeared — bug"): its scope globs, specialists and skills only mean something in the
    // workspace it was made in. The cockpit db is shared, so the row names its workspace root.
    let _ = db.execute_batch("ALTER TABLE agent_recipes ADD COLUMN workspace TEXT NOT NULL DEFAULT ''");
    // THE MERGE ON THE RECIPE (operator 2026-09-23: "on add agent, instead of just creating an agent, make it a model
    // merge"): brain × hands, as cli::Pair; the pane born from the recipe carries it in `settings.pair`
    let _ = db.execute_batch("ALTER TABLE agent_recipes ADD COLUMN pair_json TEXT NOT NULL DEFAULT ''");
    // A BOX OF ITS OWN (operator 2026-09-24): "" = the shared workspace (the default, and what keeps the running
    // preview live), "own" = a worktree per agent with a branch per episode (boxes.rs)
    let _ = db.execute_batch("ALTER TABLE agent_recipes ADD COLUMN box TEXT NOT NULL DEFAULT ''");
    seed_main(db);
}

/// THE SLUG OF THE ONE AGENT EVERY WORKSPACE HAS.
pub const MAIN: &str = "main";

/// THE MAIN AGENT (operator 2026-09-24, bug 9: "create a new agent readonly that is just the regular chat with
/// full access and no worktrees etc, a default main agent more simple and easy").
///
/// Every other agent on the roster is something the operator hired for a job: a role, a scope, a merge, a box it
/// may write in. This one is none of that. It reads the whole workspace and writes nothing, so there is nothing
/// to configure and nothing it can break — which is exactly what you want from the thing you open first, before
/// you know what you are going to ask. It is seeded with an empty workspace so it belongs to all of them, and it
/// is never archived away: a roster with no way in is a worse product than a roster with one row too many.
fn seed_main(db: &rusqlite::Connection) {
    let now = antiloki_rs::clock::now_iso_nodb();
    let _ = db.execute(
        r#"INSERT INTO agent_recipes (slug, name, icon, provider, theme, description, scope_json, specialists_json, skills_json, ask, created_at, updated_at, role, objective, archived, workspace, pair_json)
           VALUES (?1, 'Main', '◍', 'claude-code', 'antiloki', ?2, '["**"]', '[]', '[]', '', ?3, ?3, '', '', 0, '', '')
           ON CONFLICT(slug) DO NOTHING"#,
        rusqlite::params![MAIN, "reads the whole workspace, changes nothing — the one to open before you know what you are asking", now]);
    // it can never be archived: unarchiving something you cannot see is not a recovery path
    let _ = db.execute("UPDATE agent_recipes SET archived = 0 WHERE slug = ?1", rusqlite::params![MAIN]);
    // …AND ITS DESCRIPTION IS NO LONGER A LIE (operator 2026-09-27: "first is not suppose to be read-only, we dont
    // have this anymore"). The seeded line said "reads the whole workspace, changes nothing"; main is the one master
    // agent now and it works. Seeded rows keep whatever text they have unless it is that exact sentence.
    let _ = db.execute(
        "UPDATE agent_recipes SET description = ?2 WHERE slug = ?1 AND description LIKE 'reads the whole workspace, changes nothing%'",
        rusqlite::params![MAIN, "the whole workspace, one agent — fork it for a second line of work"]);
}

/// UNPIN THE PANES MAIN ALREADY HAS (2026-09-27). `open_pane` stopped forcing `chat + readonly + delegate off` onto
/// main, but a pane created BEFORE that carries those flags in its own `settings_json` and would stay a read-only
/// chat for ever — which is exactly what the operator was looking at when they reported it.
///
/// `readonly` off, `chat` on (the chrome stays), and a cheap `model` when the pane names none — on main and on every
/// fork of it. Everything else the operator chose is left alone.
pub fn unpin_main_panes(db: &rusqlite::Connection) -> Vec<Value> {
    let rows: Vec<(i64, String, String, String)> = {
        let Ok(mut st) = db.prepare("SELECT id, COALESCE(pane_kind,''), COALESCE(settings_json,'{}'), COALESCE(extra_prompt_md,'') FROM cockpit_panes WHERE (subagent_name = 'agent-main' OR subagent_name LIKE 'agent-main-%') AND status != 'closed'") else { return vec![] };
        let it = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)));
        match it { Ok(i) => i.filter_map(Result::ok).collect(), Err(_) => return vec![] }
    };
    let mut out = vec![];
    for (id, kind, sj, guidance) in rows {
        let Ok(mut v) = serde_json::from_str::<Value>(&sj) else { continue };
        let before = v.to_string();
        let Some(m) = v.as_object_mut() else { continue };
        // the chrome stays (antiChat's episodes, composer, stats), the shackle goes
        m.insert("readonly".into(), json!(false));
        m.insert("chat".into(), json!(true));
        // THE THIRD PIN (found 2026-09-27 22:55 with Playwright): `delegate: off` was forced onto main by the same
        // block, and Heimdall's route goes through goal planning, which refuses a pane with delegation off — so
        // every routed ask died with a 400 the FE swallowed ("my message go to vacuo"). On, like every other pane.
        if let Some(d) = m.get_mut("delegate").and_then(Value::as_object_mut) { d.insert("enabled".into(), json!(true)); }
        // THE CONVERSATION RUNS ON THE MID RUNG (operator 2026-09-29: "the conversation should be the main provider of the user on a mid tier model - only the plan and the work choose the
        // best merge"). This pinned the CHEAP rung ("Heimdall raises it when the ask warrants"), and the raising was `/model` typed into the chat before each message: the model the
        // operator talked to changed message by message, and after a low route it stayed on haiku. Now a pane with no model starts on the seat's MID rung, and a pane still carrying the
        // cheap rung that this very function pinned is moved up ONCE (`conv: "mid"` is the mark). After the mark, whatever the operator picks is theirs and is never touched.
        let engine = crate::cli::engine_of(&json!(kind)).unwrap_or(crate::cli::Engine::ClaudeCode);
        let mid = crate::cli::tier_model(engine, crate::cli::Tier::Mid);
        let low = crate::cli::tier_model(engine, crate::cli::Tier::Low);
        let cur = m.get("model").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        let marked = m.get("conv").and_then(Value::as_str) == Some("mid");
        match (&cur, &mid) {
            (None, Some(mid)) => { m.insert("model".into(), json!(mid)); }
            (Some(c), Some(mid)) if !marked && Some(c) == low.as_ref() => { m.insert("model".into(), json!(mid)); }
            _ => {}
        }
        m.insert("conv".into(), json!("mid"));
        // …AND THE SENTENCE THAT TOLD THE AGENT IT WAS READ-ONLY (operator 2026-09-28: "I FUCKING TOLD YOU ALL
        // AGENTS CAN EDIT NOW"). Main's pane carried the old recipe description as its OPERATOR GUIDANCE, and the
        // bake writes that verbatim into AGENTS.md/CLAUDE.md — so the most authoritative sentence the model could
        // see said it must not write, and it obeyed. Guidance the operator actually wrote is never touched.
        let clear_guidance = guidance.starts_with("reads the whole workspace, changes nothing");
        if v.to_string() == before && !clear_guidance { continue }
        // IT DECIDES, IT DOES NOT WRITE (audit 2026-09-28). `cockpit_panes` is a substrate table and
        // `system.cockpit.pane.update` owns it — and that action does more than set the columns: an
        // `extra_prompt_md` or `settings` edit re-runs the context bake, which rewrites the on-disk brief. Writing
        // the row here cleared the column and left the brief saying the old thing, which is the very bug this
        // function exists to fix. So it returns the edits and the caller dispatches them.
        let mut edit = serde_json::Map::new();
        edit.insert("pane_id".into(), json!(id));
        edit.insert("settings".into(), v.clone());
        if clear_guidance { edit.insert("extra_prompt_md".into(), json!("")); }
        out.push(Value::Object(edit));
    }
    out
}

/// …and the dispatch half, which must run where no substrate connection is held: `engine_call` re-enters the
/// pool, and a caller holding `conn.lock()` would deadlock on its own write (the 2026-09-21 single-conn incident).
pub async fn apply_unpin(state: &std::sync::Arc<crate::AppState>, edits: Vec<Value>) -> usize {
    let mut n = 0;
    for e in edits {
        if crate::dispatch::engine_call(state, "system.cockpit.pane.update", e, crate::chat_pane::ctx()).await.is_ok() { n += 1 }
    }
    n
}
/// The active workspace's root — the key a recipe is filed under.
fn active_root(db: &rusqlite::Connection) -> String { antiloki_rs::engine_workspace_root::getWorkspaceRoot(db).as_str().unwrap_or("").to_string() }
fn row_out(r: &Value) -> Value {
    let arr = |k: &str| r.get(k).and_then(Value::as_str).and_then(|t| serde_json::from_str::<Value>(t).ok()).unwrap_or(json!([]));
    let slug = r.get("slug").and_then(Value::as_str).map(|k| k.rsplit("::").next().unwrap_or(k).to_string()).unwrap_or_default();
    let pair = r.get("pair_json").and_then(Value::as_str).filter(|t| !t.is_empty()).and_then(|t| serde_json::from_str::<Value>(t).ok()).unwrap_or(Value::Null);
    json!({ "slug": slug, "name": r.get("name"), "icon": r.get("icon"), "provider": r.get("provider"), "theme": r.get("theme"), "role": role_of(r), "objective": r.get("objective"), "box": r.get("box"), "archived": r.get("archived").and_then(Value::as_i64).unwrap_or(0) != 0, "description": r.get("description"), "scope": arr("scope_json"), "specialists": arr("specialists_json"), "skills": arr("skills_json"), "ask": r.get("ask"), "pair": pair, "created_at": r.get("created_at"), "updated_at": r.get("updated_at") })
}
/// A recipe from a request body, validated. Pure.
/// the role keys a recipe may carry — the catalogue's own, so a typo cannot invent a role. Pure.
fn role_keys() -> Vec<&'static str> { crate::roles::ROLES.iter().map(|r| r.key).collect() }
thread_local! { static ROLE_KEY_CACHE: Vec<&'static str> = role_keys(); }
#[allow(non_snake_case)]
fn ROLE_KEYS_contains(k: &str) -> bool { ROLE_KEY_CACHE.with(|v| v.contains(&k)) }

/// EVERY AGENT HAS A ROLE, including the ones made before the column existed: a stored role when it names a real
/// one, else the role its name and purpose ask for. Pure apart from reading the row. */
pub fn role_of(r: &Value) -> String {
    // MAIN HAS NO ROLE (2026-09-24, bug 9). Guessing one from its name and purpose is what the fallback below is
    // for — agents hired before the column existed — and applied here it invents a job for the one agent that was
    // deliberately not hired to do one. It read as "Backender", which is the opposite of what it is.
    if r.get("slug").and_then(Value::as_str).map(|x| x.rsplit("::").next().unwrap_or(x)) == Some(MAIN) { return String::new() }
    let stored = r.get("role").and_then(Value::as_str).unwrap_or("").trim().to_lowercase();
    if ROLE_KEYS_contains(stored.as_str()) { return stored; }
    let name = r.get("name").and_then(Value::as_str).unwrap_or("");
    let about = r.get("description").and_then(Value::as_str).unwrap_or("");
    crate::roles::suggest_role(&format!("{name} {about}")).key.to_string()
}

pub fn parse_recipe(b: &Value) -> Result<Value, String> {
    let name = s(b.get("name")).trim().to_string();
    if name.is_empty() { return Err("a name is required".into()); }
    let slug = { let given = s(b.get("slug")); if given.is_empty() { slugify(&name) } else { slugify(&given) } };
    if slug.is_empty() { return Err("the name needs at least one letter or digit".into()); }
    // the pair: a name or the shape (cli::Pair::of_settings); with one, the provider — the pane you type into — is the BRAIN's engine
    let pair = b.get("pair").filter(|p| !p.is_null()).and_then(|p| crate::cli::Pair::of_settings(&json!({ "pair": p })));
    let provider = match &pair { Some(p) => p.brain.key().to_string(), None => s(b.get("provider")) };
    if !providers().contains(&provider.as_str()) { return Err(format!("provider must be one of {} — or a pair whose brain is one of them", providers().join(", "))); }
    // an icon is either an EMOJI badge (capped, so a paragraph can't become one) or a FACE SPEC, "face:<shape>:<hex>",
    // which is 16-odd characters and was being cut to the word "face" — every agent made from a role then wore that
    // word under its avatar in the roster (2026-09-23).
    let icon = {
        let i = s(b.get("icon"));
        if i.trim().is_empty() { "🤖".to_string() }
        else if i.starts_with("face:") { i.chars().take(40).collect() }
        else { i.chars().take(4).collect() }
    };
    // no house theme (operator 2026-10-11): an agent with no theme of its own wears its engine's look; the screen reads an empty or a retired theme that way
    let theme = { let t = s(b.get("theme")); if t == "antiloki" { String::new() } else { t } };
    let description = s(b.get("description")).chars().take(600).collect::<String>();
    // EVERY AGENT HAS A ROLE. Given one, we keep it if it names a real role; given none, the purpose picks it — the
    // same `suggest_role` the creation wizard uses, so a typed agent and a suggested one land on the same key.
    let role = {
        let given = s(b.get("role")).trim().to_lowercase();
        if ROLE_KEYS_contains(given.as_str()) { given } else { crate::roles::suggest_role(&format!("{name} {description}")).key.to_string() }
    };
    let objective = crate::roles::Objective::of(s(b.get("objective")).trim()).map(|o| o.key().to_string()).unwrap_or_default();
    Ok(json!({ "slug": slug, "name": name, "icon": icon, "provider": provider, "theme": theme, "role": role, "objective": objective, "pair": pair.map(|p| serde_json::to_value(p).unwrap_or(Value::Null)).unwrap_or(Value::Null), "description": description, "scope": strs(b.get("scope")), "specialists": strs(b.get("specialists")), "skills": strs(b.get("skills")), "ask": s(b.get("ask")).chars().take(2000).collect::<String>(),
        // A BOX OF ITS OWN IS NOT A CHOICE (operator 2026-09-25: "I thought we decide for one path not optionals
        // paths. Worktree were suppose to be for agents … dont make a choice optional about this"). It used to be
        // opt-in, defaulting to the shared workspace because the live preview only showed changes made there —
        // and the preview resolves a pane_id to that pane's own worktree, so that reason never held. An absent
        // field is a box; only an explicit "" is the shared workspace, which exists solely so the agents hired
        // before this still load and update as they were.
        "box": match b.get("box").and_then(Value::as_str) { Some("own") | None => "own", Some(_) => "" } }))
}

/// `GET /api/v1/cockpit/agents/recipes`
pub async fn list(state: Arc<AppState>) -> Response {
    let conn = state.pick();
    let rows = tokio::task::spawn_blocking(move || {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner()); ensure(&c);
        let root = active_root(&c);
        antiloki_rs::db_json::query_rows(&c, "SELECT * FROM agent_recipes WHERE workspace = ? ORDER BY created_at ASC", &[json!(root)]).unwrap_or(json!([]))
    }).await.unwrap_or(json!([]));
    Json(json!({ "ok": true, "recipes": rows.as_array().map(|a| a.iter().map(row_out).collect::<Vec<_>>()).unwrap_or_default() })).into_response()
}
/// `POST /api/v1/cockpit/agents/recipes` — upsert by slug.
pub async fn save(state: Arc<AppState>, body: Bytes) -> Response {
    let b = body_json(&body);
    let r = match parse_recipe(&b) { Ok(r) => r, Err(e) => return (StatusCode::CREATED, Json(json!({ "ok": false, "error": e }))).into_response() };
    let conn = state.pick();
    let out = tokio::task::spawn_blocking(move || {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner()); ensure(&c);
        let now = antiloki_rs::clock::now_iso(&c);
        let root = active_root(&c);
        let j = |k: &str| json!(serde_json::to_string(r.get(k).unwrap_or(&json!([]))).unwrap_or_default());
        // the slug is unique per workspace: the same name in two workspaces is two recipes
        let key = format!("{}::{}", root, r["slug"].as_str().unwrap_or(""));
        antiloki_rs::db_json::run_write(&c, "INSERT INTO agent_recipes (slug, workspace, name, icon, provider, theme, description, scope_json, specialists_json, skills_json, ask, created_at, updated_at, pair_json, role, objective, box) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(slug) DO UPDATE SET name=excluded.name, icon=excluded.icon, provider=excluded.provider, theme=excluded.theme, description=excluded.description, scope_json=excluded.scope_json, specialists_json=excluded.specialists_json, skills_json=excluded.skills_json, ask=excluded.ask, pair_json=excluded.pair_json, role=excluded.role, objective=excluded.objective, box=excluded.box, updated_at=excluded.updated_at",
            &[json!(key), json!(root), r["name"].clone(), r["icon"].clone(), r["provider"].clone(), r["theme"].clone(), r["description"].clone(), j("scope"), j("specialists"), j("skills"), r["ask"].clone(), json!(now), json!(now), json!(if r["pair"].is_null() { String::new() } else { serde_json::to_string(&r["pair"]).unwrap_or_default() }), r["role"].clone(), r["objective"].clone(), r["box"].clone()])?;
        antiloki_rs::db_json::query_rows(&c, "SELECT * FROM agent_recipes WHERE slug = ?", &[json!(key)])
    }).await.unwrap_or_else(|e| Err(e.to_string()));
    match out { Ok(rows) => (StatusCode::CREATED, Json(json!({ "ok": true, "recipe": rows.as_array().and_then(|a| a.first()).map(row_out) }))).into_response(), Err(e) => (StatusCode::CREATED, Json(json!({ "ok": false, "error": e }))).into_response() }
}
/// `POST /api/v1/cockpit/agents/recipes/archive {slug, archived?}` — take an agent off the roster, or bring it back.
/// NOT a delete: the episodes, the runs and the money under its face are the record of work that happened, and an
/// operator who is done with an agent is not asking to lose that (operator 2026-09-23).
pub async fn archive(state: Arc<AppState>, body: Bytes) -> Response {
    let b = body_json(&body);
    let slug = s(b.get("slug"));
    let on = b.get("archived").and_then(Value::as_bool).unwrap_or(true);
    if slug.trim().is_empty() { return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "a slug is required" }))).into_response(); }
    let conn = state.pick();
    let n = tokio::task::spawn_blocking(move || {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner()); ensure(&c);
        let key = format!("{}::{}", active_root(&c), slug);
        antiloki_rs::db_json::run_write(&c, "UPDATE agent_recipes SET archived = ?, updated_at = ? WHERE slug = ?", &[json!(on as i64), json!(crate::cli::now_ms().to_string()), json!(key)]).map(|(n, _)| n).unwrap_or(0)
    }).await.unwrap_or(0);
    (StatusCode::CREATED, Json(json!({ "ok": n > 0, "archived": on }))).into_response()
}
/// `POST /api/v1/cockpit/agents/recipes/delete {slug}`
pub async fn delete(state: Arc<AppState>, body: Bytes) -> Response {
    let slug = s(body_json(&body).get("slug"));
    let conn = state.pick();
    let n = tokio::task::spawn_blocking(move || { let c = conn.lock().unwrap_or_else(|p| p.into_inner()); ensure(&c); let key = format!("{}::{}", active_root(&c), slug); antiloki_rs::db_json::run_write(&c, "DELETE FROM agent_recipes WHERE slug = ?", &[json!(key)]).map(|(n, _)| n).unwrap_or(0) }).await.unwrap_or(0);
    (StatusCode::CREATED, Json(json!({ "ok": n > 0 }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE SIX BUGS OF 2026-09-27 NIGHT, half of them: main's panes carried `chat + readonly` from before, and a
    /// pane with no model fell through to the CLI's default — opus, $0.0797 for "hi". The migration strips the
    /// shackle, keeps the chrome, and pins the cheap rung — on main and on every fork of it, and on nothing else.
    /// stands in for `system.cockpit.pane.update` so the decision can be tested without a dispatcher
    fn apply_for_test(db: &rusqlite::Connection, e: &Value) {
        let id = e["pane_id"].as_i64().unwrap();
        let _ = db.execute("UPDATE cockpit_panes SET settings_json = ?2 WHERE id = ?1", rusqlite::params![id, e["settings"].to_string()]);
        if e.get("extra_prompt_md").is_some() {
            let _ = db.execute("UPDATE cockpit_panes SET extra_prompt_md = ?2 WHERE id = ?1", rusqlite::params![id, ""]);
        }
    }
    #[test]
    fn unpinning_main_keeps_the_chat_chrome_drops_readonly_and_pins_the_mid_rung() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY, subagent_name TEXT, pane_kind TEXT, status TEXT NOT NULL DEFAULT 'idle', settings_json TEXT, extra_prompt_md TEXT)").unwrap();
        db.execute_batch(r#"INSERT INTO cockpit_panes (id, subagent_name, pane_kind, settings_json) VALUES
            (25, 'agent-main', 'claude-code', '{"agent":"main","chat":true,"readonly":true,"delegate":{"enabled":false},"preview":{"enabled":false}}'),
            (50, 'agent-main-sly-twin', 'claude-code', '{"agent":"main-sly-twin","title":"sly twin","readonly":false}'),
            (51, 'agent-main-kimi', 'opencode', '{"agent":"main-kimi","model":"opencode/kimi-k2.7-code"}'),
            (34, 'agent-patcher', 'claude-code', '{"agent":"patcher","chat":true,"readonly":true}'),
            (70, 'agent-main-old', 'claude-code', '{"agent":"main-old","chat":true,"model":"haiku"}'),
            (71, 'agent-main-chose', 'claude-code', '{"agent":"main-chose","chat":true,"readonly":false,"delegate":{"enabled":true},"model":"haiku","conv":"mid"}');
            INSERT INTO cockpit_panes (id, subagent_name, pane_kind, status, settings_json) VALUES (60, 'agent-main-gone', 'claude-code', 'closed', '{"readonly":true}')"#).unwrap();
        // main's pane carries the old recipe description as guidance — the sentence that made the agent refuse
        let _ = db.execute("UPDATE cockpit_panes SET extra_prompt_md = 'reads the whole workspace, changes nothing — the one to open before you know what you are asking' WHERE id = 25", []);
        let edits = unpin_main_panes(&db);
        assert_eq!(edits.len(), 4, "main, its live forks and the pane still on the old cheap pin - not the closed one, not another agent, not one already marked");
        // the function DECIDES; the caller dispatches through the action that owns the table
        assert!(edits.iter().any(|e| e["pane_id"] == json!(25) && e["extra_prompt_md"] == json!("")),
                "main's stale guidance is part of the edit, not a second direct write");
        for e in &edits { apply_for_test(&db, e); }
        let get = |id: i64| -> Value { let s: String = db.query_row("SELECT settings_json FROM cockpit_panes WHERE id = ?1", [id], |r| r.get(0)).unwrap(); serde_json::from_str(&s).unwrap() };
        let low = crate::cli::tier_model(crate::cli::Engine::ClaudeCode, crate::cli::Tier::Mid).expect("claude has a mid rung");
        let main = get(25);
        assert_eq!(main["readonly"], json!(false), "it can write");
        assert_eq!(main["chat"], json!(true), "…and keeps antiChat's chrome, episodes and all");
        assert_eq!(main["model"], json!(low), "and talks on the MID rung, not on whatever the CLI defaults to and not on the cheapest");
        assert_eq!(get(70)["model"], json!(low), "a pane still on the cheap pin this function made is moved up once");
        assert_eq!(get(71)["model"], json!("haiku"), "…and after the mark a cheap choice is the operator's own");
        assert_eq!(main["delegate"], json!({ "enabled": true }), "delegation back on — Heimdall's route needs it, and off was the pin, not a choice");
        let fork = get(50);
        assert_eq!((fork["readonly"].clone(), fork["chat"].clone(), fork["model"].clone()), (json!(false), json!(true), json!(low)));
        assert_eq!(fork["title"], json!("sly twin"), "its name survives");
        assert_eq!(main["preview"], json!({ "enabled": false }), "the operator's other choices are untouched");
        assert_eq!(get(51)["model"], json!("opencode/kimi-k2.7-code"), "a model the operator chose is never overwritten");
        assert_eq!(get(34), json!({ "agent": "patcher", "chat": true, "readonly": true }), "another agent's pane is not main's business");
        assert_eq!(get(60), json!({ "readonly": true }), "a closed pane is history, not a candidate");
        assert!(unpin_main_panes(&db).is_empty(), "every boot runs it — the second time has nothing to change");
        // the guidance that told the agent it may not write is gone (operator 2026-09-28)
        let g: String = db.query_row("SELECT COALESCE(extra_prompt_md,'') FROM cockpit_panes WHERE id = 25", [], |r| r.get(0)).unwrap_or_default();
        assert!(!g.contains("changes nothing"), "the brief still tells main it changes nothing: {g:?}");
    }

    // BUG 9 (operator 2026-09-24): "create a new agent readonly that is just the regular chat with full access
    // and no worktrees etc, a default main agent more simple and easy."
    #[test]
    fn every_workspace_has_a_main_agent_and_cannot_lose_it() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        ensure(&db);
        let one = |sql: &str| antiloki_rs::db_json::query_rows(&db, sql, &[]).unwrap_or(json!([]));
        let rows = one("SELECT slug, name, scope_json, workspace, archived, pair_json, role FROM agent_recipes WHERE slug = 'main'");
        let r = rows.as_array().unwrap();
        assert_eq!(r.len(), 1, "seeded exactly once");
        assert_eq!(r[0]["name"], json!("Main"));
        assert_eq!(r[0]["scope_json"], json!("[\"**\"]"), "the whole workspace, since it cannot write to any of it");
        assert_eq!(r[0]["workspace"], json!(""), "belongs to every workspace, not one");
        assert_eq!(r[0]["pair_json"], json!(""), "no merge to choose — there is nothing to be cheaper about");
        assert_eq!(r[0]["role"], json!(""), "and no role: it was not hired for a job");
        // …and the roster must not INVENT one for it: role_of guesses from the name and purpose for agents made
        // before the column existed, and on Main that guess came back "backender", the opposite of what it is
        assert_eq!(role_of(&r[0]), "", "no guessed role either");
        assert_eq!(role_of(&json!({ "slug": "ws::main", "name": "Main", "description": "reads the whole workspace" })), "", "however the slug is qualified");
        assert_ne!(role_of(&json!({ "slug": "other", "name": "Backender", "description": "the server and its endpoints" })), "", "a real agent still gets one");

        // running ensure again is what happens on every serve boot — it must not duplicate or reset the row
        ensure(&db);
        assert_eq!(one("SELECT slug FROM agent_recipes WHERE slug = 'main'").as_array().unwrap().len(), 1);

        // archived away and brought back: an operator who hides the way in has no way to unhide it
        db.execute("UPDATE agent_recipes SET archived = 1 WHERE slug = 'main'", []).unwrap();
        ensure(&db);
        assert_eq!(one("SELECT archived FROM agent_recipes WHERE slug = 'main'").as_array().unwrap()[0]["archived"], json!(0));
    }

    #[test]
    fn slugs_and_validation() {
        assert_eq!(slugify("  Docs Writer (React) "), "docs-writer-react");
        assert_eq!(slugify("✦ Reviewer!"), "reviewer");
        let r = parse_recipe(&json!({ "name": "Docs Writer", "provider": "claude-code", "icon": "📝", "scope": ["docs/**", " "], "specialists": ["axis:docs"], "skills": ["antiloki-file-review"] })).unwrap();
        assert_eq!(r["slug"], "docs-writer"); assert_eq!(r["theme"], ""); assert_eq!(r["scope"], json!(["docs/**"]));
        assert!(parse_recipe(&json!({ "name": "x", "provider": "gpt" })).unwrap_err().contains("provider must be one of"));
        assert!(parse_recipe(&json!({ "provider": "codex" })).unwrap_err().contains("name is required"));
        assert_eq!(parse_recipe(&json!({ "name": "R", "provider": "codex" })).unwrap()["icon"], "🤖");
        // a face spec survives whole; an emoji is still capped; neither becomes the word "face"
        assert_eq!(parse_recipe(&json!({ "name": "R", "provider": "codex", "icon": "face:egg:#7c5cf0" })).unwrap()["icon"], "face:egg:#7c5cf0");
        assert_eq!(parse_recipe(&json!({ "name": "R", "provider": "codex", "icon": "a very long label indeed" })).unwrap()["icon"], "a ve");

        // EVERY AGENT HAS A ROLE (2026-09-23). A real one is kept; anything else is decided by what the agent is for.
        let kept = parse_recipe(&json!({ "name": "R", "provider": "codex", "role": "tester" })).unwrap();
        assert_eq!(kept["role"], "tester", "a real role is kept as written");
        let upper = parse_recipe(&json!({ "name": "R", "provider": "codex", "role": "TESTER" })).unwrap();
        assert_eq!(upper["role"], "tester", "and is not case-sensitive");
        let made_up = parse_recipe(&json!({ "name": "R", "provider": "codex", "role": "wizard", "description": "writes the tests and keeps the suite green" })).unwrap();
        assert_eq!(made_up["role"], "tester", "an invented role falls back to what the purpose asks for");
        let none = parse_recipe(&json!({ "name": "Docs Writer", "provider": "codex", "description": "keeps the docs true" })).unwrap();
        assert!(!none["role"].as_str().unwrap_or("").is_empty(), "no role given is still a role");

        // the objective rides along, and only a real one
        assert_eq!(parse_recipe(&json!({ "name": "R", "provider": "codex", "objective": "faster" })).unwrap()["objective"], "faster");
        assert_eq!(parse_recipe(&json!({ "name": "R", "provider": "codex", "objective": "quickest" })).unwrap()["objective"], "");
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;
    #[test]
    fn a_recipe_with_a_pair_takes_its_provider_from_the_brain() {
        // PORTED off the withdrawn Zen merges (2026-09-27)
        let r = parse_recipe(&json!({ "name": "Cold outreach operator", "scope": ["docs/**"], "pair": "claude-composer" })).unwrap();
        assert_eq!(r["provider"], "claude-code"); assert_eq!(r["pair"]["hands"], "cursor");
        let r = parse_recipe(&json!({ "name": "Scout", "scope": ["a"], "pair": { "name": "mine", "brain": { "engine": "cursor", "model": "cursor-grok-4.6-high" }, "hands": { "engine": "cursor", "model": "composer-2.5" } } })).unwrap();
        assert_eq!((r["provider"].as_str(), r["pair"]["brain_model"].as_str()), (Some("cursor"), Some("cursor-grok-4.6-high")));
        // no pair: the provider as before
        assert_eq!(parse_recipe(&json!({ "name": "x", "provider": "codex", "scope": ["a"] })).unwrap()["provider"], "codex");
        assert!(parse_recipe(&json!({ "name": "x", "provider": "gpt", "scope": ["a"] })).is_err());
    }
}
