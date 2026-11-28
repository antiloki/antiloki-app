//! The generic passthrough handler — one function standing in for 237 NestJS controller methods.
//!
//! Port of `EngineService.dispatchFromHttp()` plus the response conventions W0 measured off the
//! live BE. Three of those conventions are invisible in the controller source and were only
//! discoverable by probing, so they are called out where they are implemented:
//!   1. POST answers 201, GET answers 200 (NestJS defaults).
//!   2. Typed engine failures use the `{error:{code,skill,cause,context}}` envelope.
//!   3. Actor attribution is derived from the X-FE-Session header, never trusted from the body.

use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

use crate::error::{code_to_status, to_antiloki_error, unhandled_body};

/// Cheap indexed MAX on the PK, called on every dispatch — a direct query, not an action,
/// exactly as the napi bridge does it.
fn last_timeline_id(conn: &rusqlite::Connection) -> i64 {
    conn.query_row("SELECT MAX(id) AS m FROM sacred_timeline", [], |r| r.get::<_, Option<i64>>(0))
        .map(|v| v.unwrap_or(0))
        .unwrap_or(0)
}

// A panic inside a skill must NOT brick the server. `Mutex::lock()` returns Err forever once a
// holder panicked, so the original code answered "engine connection poisoned" to EVERY later
// request — one bad skill and the process was dead weight. `into_inner()` takes the lock anyway.
// This is what the BE effectively does too (an exception there does not kill the connection),
// except that in the BE a RUST panic crosses napi and aborts the whole Node process instead.
pub struct AppState {
    /// FEAT-BE-WAKE-ON-EVENT: an operator dispatch may have just made a card actionable, so the
    /// next tick is pulled forward instead of waiting out dormancy. Set once at boot, after the
    /// Ticker exists (it holds the AppState, so this is the seam that avoids the cycle at
    /// construction). Both live for the process lifetime.
    pub waker: std::sync::OnceLock<Arc<dyn Fn() + Send + Sync>>,
    /// SSE hub. A dispatch that appended to sacred_timeline pings connected clients so the FE
    /// refetches immediately instead of waiting for the next tick.
    pub events: Arc<crate::events::EventsHub>,
    /// The primary connection behind a Mutex — the tick loop, the prompt catalog and any caller
    /// that wants "the" connection. Dispatches prefer a free member of `pool` (below).
    pub conn: Arc<Mutex<rusqlite::Connection>>,
    /// THE DISPATCH POOL (2026-09-19): more connections to the same WAL database, opened exactly
    /// like `conn`. A page burst used to queue every read head-of-line behind one Mutex — the
    /// X-ray tab's ~40 reads took seconds each, the FE's budget aborted the tail ("registry
    /// unavailable", "loading the agents…" for ten seconds). WAL lets readers run concurrently;
    /// writers still serialize inside SQLite (busy_timeout), so any action may run on any member.
    /// Empty in tests that build the state by hand — `pick()` then falls back to `conn`.
    pub pool: Vec<Arc<Mutex<rusqlite::Connection>>>,
    /// The path the connections were opened with — `rebind()` reopens them from it.
    pub db_path: String,
}

/// THE WORKSPACE EPOCH (2026-10-11): moves every time the serve rebinds its connections to another workspace. Everything the serve keeps in memory per PANE ID (live agents, event counters,
/// each pane's folder) belongs to the workspace it was made in, and pane ids are numbered per workspace: project A's pane 1 and project B's pane 1 are different panes with the same key.
/// The agent registry reads this to drop what belongs to the workspace that is gone (`AgentRegistry::sync_to`).
/// PER SERVE, KEYED BY ITS DB PATH: a process can hold several (the journeys run dozens), and one's workspace switch must never reset another's agents.
fn epochs() -> &'static Mutex<std::collections::HashMap<String, u64>> {
    static E: std::sync::OnceLock<Mutex<std::collections::HashMap<String, u64>>> = std::sync::OnceLock::new();
    E.get_or_init(Default::default)
}
pub fn workspace_epoch(db_path: &str) -> u64 { epochs().lock().unwrap_or_else(|p| p.into_inner()).get(db_path).copied().unwrap_or(0) }
/// PANE NUMBERS THAT DO NOT COLLIDE ACROSS WORKSPACES (operator 2026-11-10: "e2escratch and ministore are completely apart spaces, but they are using the same panes"). Every workspace numbered its panes from 1, and a terminal, a layout leaf, a
/// hook file name are all keyed by the number alone, so project A's pane 11 and project B's were the same thing to them. A workspace's NEW panes now start from a base of its own (the same folder always gets the same base), so the
/// numbers of different workspaces differ; panes made before keep the numbers they have. Pure: the base is a function of the root.
pub fn pane_id_base(root: &str) -> i64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in root.trim_end_matches(['/', '\\']).replace('\\', "/").to_lowercase().bytes() { h ^= b as u64; h = h.wrapping_mul(0x100000001b3); }
    (h % 9_000) as i64 * 100_000 + 100_000
}
/// Raise this workspace's pane counter to its base, once (an existing higher counter is left alone). Returns the counter after.
pub fn ensure_pane_id_base(c: &rusqlite::Connection, root: &str) -> Option<i64> {
    if root.trim().is_empty() || c.prepare("SELECT 1 FROM sqlite_master WHERE name = 'cockpit_panes'").ok()?.exists([]).ok()? == false { return None; }
    let base = pane_id_base(root);
    let have: Option<i64> = c.query_row("SELECT seq FROM sqlite_sequence WHERE name = 'cockpit_panes'", [], |r| r.get(0)).ok();
    match have {
        Some(n) if n >= base => Some(n),
        Some(_) => { c.execute("UPDATE sqlite_sequence SET seq = ?1 WHERE name = 'cockpit_panes'", [base]).ok()?; Some(base) }
        None => { c.execute("INSERT INTO sqlite_sequence (name, seq) VALUES ('cockpit_panes', ?1)", [base]).ok()?; Some(base) }
    }
}
fn bump_epoch(db_path: &str) { *epochs().lock().unwrap_or_else(|p| p.into_inner()).entry(db_path.to_string()).or_insert(0) += 1; }

impl AppState {
    /// REBIND ON A WORKSPACE SWITCH (2026-09-20, found the hard way: the sidebar said miniStore while
    /// every dispatch still hit react-main — a custom agent and the file tree from the other workspace
    /// showed up). `openDb` redirects to the ACTIVE workspace's db set only when a connection is
    /// opened, and the serve opens its connections once, at boot. After `system.workspace.activate`
    /// every connection (the primary and the pool) is reopened, so the next dispatch lands in the
    /// workspace the operator just chose. Each Mutex keeps its identity; only the connection inside changes.
    ///
    /// A CONNECTION THAT IS BUSY IS REBOUND WHEN IT IS FREE, NOT AWAITED (operator 2026-09-29: "rebuild working for 70 minutes, blocked switching away from it"; acceptance test J9b).
    /// The rebuild of a workspace is ONE engine action that holds a pool connection for its whole run - minutes on a big tree - and this used to `lock()` every slot in turn, so
    /// `workspaces/activate`, which calls it, waited for the rebuild of the workspace it was leaving. Now a free slot is swapped at once and a held one is handed to a detached waiter
    /// (`defer_rebind`) that swaps it the moment the action lets go; `pick()` does not hand a pending slot out in the meantime. This never blocks the caller, which also makes it safe
    /// to call from a request path (serve-single-conn-deadlock). The rebuild itself keeps writing to the workspace it was started on, which is what it should do. Returns how many
    /// connections were rebound NOW.
    pub fn rebind(&self) -> Result<usize, String> {
        let open = || antiloki_rs::engine_open_db::openDb(&serde_json::json!(self.db_path), &serde_json::json!({ "readOnly": false }));
        let mut n = 0usize;
        for slot in std::iter::once(&self.conn).chain(self.pool.iter()) {
            if rebind_is_pending(slot) { continue }   // already waiting for its action to end: it will open the registry's CURRENT workspace when it gets there
            let fresh = open()?;
            match slot.try_lock() {
                Ok(mut g) => { *g = fresh; n += 1; }
                Err(std::sync::TryLockError::Poisoned(p)) => { *p.into_inner() = fresh; n += 1; }
                Err(std::sync::TryLockError::WouldBlock) => { drop(fresh); defer_rebind(slot.clone(), self.db_path.clone()); }
            }
        }
        bump_epoch(&self.db_path);
        self.ensure_pane_base();
        Ok(n)
    }
    /// A free connection for a dispatch: the first unlocked pool member that is not waiting to be rebound, else the primary (which
    /// blocks - the same behaviour as before the pool existed).
    /// The open workspace's panes number from a base of its own (see `pane_id_base`).
    pub fn ensure_pane_base(&self) {
        let c = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let root = antiloki_rs::engine_workspace_root::getWorkspaceRoot(&c).as_str().unwrap_or("").to_string();
        let _ = ensure_pane_id_base(&c, &root);
    }
    pub fn pick(&self) -> Arc<Mutex<rusqlite::Connection>> {
        for c in &self.pool {
            if rebind_is_pending(c) { continue }
            if let Ok(g) = c.try_lock() { drop(g); return c.clone(); }
        }
        self.conn.clone()
    }
}

/// The connections whose rebind is waiting for the action that holds them (by address; the Arcs live as long as the state).
fn pending_rebinds() -> &'static Mutex<std::collections::HashSet<usize>> {
    static P: std::sync::OnceLock<Mutex<std::collections::HashSet<usize>>> = std::sync::OnceLock::new();
    P.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}
fn slot_key(slot: &Arc<Mutex<rusqlite::Connection>>) -> usize { Arc::as_ptr(slot) as usize }
fn rebind_is_pending(slot: &Arc<Mutex<rusqlite::Connection>>) -> bool { pending_rebinds().lock().unwrap_or_else(|p| p.into_inner()).contains(&slot_key(slot)) }
/// How many connections are still waiting to be rebound (the busy ones). Tests and the log read it.
pub fn rebinds_pending() -> usize { pending_rebinds().lock().unwrap_or_else(|p| p.into_inner()).len() }
/// Swap `slot` for a fresh connection the moment whoever holds it lets go. One detached thread per busy slot (at most the pool's size, because a slot already pending is not queued again);
/// the connection is opened AFTER the lock is won, so it binds to the workspace the registry names THEN, not the one that was active when the switch began.
fn defer_rebind(slot: Arc<Mutex<rusqlite::Connection>>, db_path: String) {
    let key = slot_key(&slot);
    if !pending_rebinds().lock().unwrap_or_else(|p| p.into_inner()).insert(key) { return }
    std::thread::spawn(move || {
        let mut g = slot.lock().unwrap_or_else(|p| p.into_inner());
        match antiloki_rs::engine_open_db::openDb(&serde_json::json!(db_path), &serde_json::json!({ "readOnly": false })) {
            Ok(fresh) => { *g = fresh; eprintln!("antiloki-serve: a connection that was busy during the workspace switch is rebound now"); }
            Err(e) => eprintln!("antiloki-serve: deferred rebind failed - {e}"),
        }
        drop(g);
        pending_rebinds().lock().unwrap_or_else(|p| p.into_inner()).remove(&key);
    });
}

/// A workspace action that changes which workspace the connections must be bound to: switching to one, and REMOVING one (operator 2026-09-30:
/// "action 'system.workspace.add' not found on trying to create a new workspace" - the active project had been removed and its folder trashed, but
/// the connections stayed on its database, whose action table was gone, so every action - even the one that adds a workspace - was "not found").
pub fn changes_workspace(action: &str) -> bool { matches!(action, "system.workspace.activate" | "system.workspace.remove") }

/// True when the file a connection was opened on no longer exists (the project folder was moved, trashed or deleted underneath the serve). The
/// connection still reads the unlinked inode, which is how a dead workspace keeps answering with a database nobody can see. Pure on the connection.
pub fn db_file_gone(c: &rusqlite::Connection) -> bool {
    let path: Option<String> = c.query_row("SELECT file FROM pragma_database_list WHERE name = 'main'", [], |r| r.get(0)).ok();
    match path { Some(p) if !p.is_empty() => !std::path::Path::new(&p).exists(), _ => false }
}

impl AppState {
    /// SELF-HEAL: if the workspace the connections are bound to vanished from disk, rebind to whatever the registry says is active now (the
    /// self-hosting default when nothing is). Checked at most once every 2 s, and never blocks on a busy primary connection.
    pub fn heal_if_gone(&self) {
        use std::sync::atomic::{AtomicI64, Ordering};
        static LAST: AtomicI64 = AtomicI64::new(0);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        if now - LAST.load(Ordering::Relaxed) < 2 { return; }
        LAST.store(now, Ordering::Relaxed);
        let gone = match self.conn.try_lock() { Ok(g) => db_file_gone(&g), Err(_) => false };
        if gone {
            match self.rebind() { Ok(n) => eprintln!("antiloki-serve: the active workspace's folder is gone — {n} connection(s) rebound to the registry's active workspace"), Err(e) => eprintln!("antiloki-serve: rebind after a vanished workspace failed — {e}") }
        }
    }
}

/// A REQUEST FOR ANOTHER WORKSPACE (operator 2026-11-10: "make x-ray work for multiple spaces at the same time with a filter per space"). The serve's connections are bound to the ACTIVE workspace, and the
/// engine's own actions read "the active workspace" from the database they are given, so the way to answer for another one is to give them ITS database set: `<root>/.antiloki/db/*.sqlite`, opened by
/// path (every workspace's own state carries its own row as the active one). Nothing is switched: the Cowork screen keeps its workspace while X-ray reads, rebuilds or edits the laws of another. The header
/// is the root path; it is honoured only for a workspace the registry knows and whose database exists, and the connection is kept for the next request. `None` means "the active workspace": no header, the
/// active one named, or a folder that is not a workspace here.
pub const WORKSPACE_HEADER: &str = "x-antiloki-workspace";
pub fn workspace_conn(state: &AppState, root: &str) -> Option<Arc<Mutex<rusqlite::Connection>>> {
    static CONNS: std::sync::OnceLock<Mutex<std::collections::HashMap<String, Arc<Mutex<rusqlite::Connection>>>>> = std::sync::OnceLock::new();
    let root = root.trim().trim_end_matches(['/', '\\']);
    if root.is_empty() { return None; }
    let state_db = std::path::Path::new(root).join(".antiloki").join("db").join("state.sqlite");
    // A TREE antiloki cut for a thread is the project's codebase being worked on (the operator 2026-11-19): X-ray can read it as a space of its own. Its database is made on first use (`.antiloki` is never part of the work).
    let is_tree = crate::thread_tree::is_tree_dir(root);
    if !state_db.exists() { if !is_tree { return None; } let _ = std::fs::create_dir_all(state_db.parent()?); if let Ok(c) = antiloki_rs::engine_open_db::openDb(&serde_json::json!({ "statePath": state_db.to_string_lossy() }), &serde_json::json!({ "readOnly": false })) {
            // the tree is the workspace of its own database: without this row its root is the substrate's, and an index of the tree would read the wrong folder
            let name = std::path::Path::new(root).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "tree".to_string());
            let _ = c.execute("INSERT OR IGNORE INTO workspaces (name, root_path, vcs, is_active, db_dir) VALUES (?1, ?2, 'git', 1, ?3)", rusqlite::params![name, root, state_db.parent().map(|d| d.to_string_lossy().to_string())]);
        }
        if !state_db.exists() { return None; } }
    let canon = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let key = format!("{}|{}", state.db_path, canon(&state_db).to_string_lossy());
    // the workspace the connections are bound to already: the ordinary path serves it
    let active = { let g = state.conn.lock().unwrap_or_else(|p| p.into_inner()); g.query_row("PRAGMA database_list", [], |r| r.get::<_, String>(2)).ok() };
    if active.map(|a| canon(std::path::Path::new(&a)) == canon(&state_db)).unwrap_or(false) { return None; }
    let mut map = CONNS.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner());
    if let Some(c) = map.get(&key) { return Some(c.clone()); }
    // a workspace the registry does not know is not one to open
    // (the list the Spaces switcher shows: the machine's registry, whichever workspace the connections are bound to)
    let known = antiloki_rs::system_workspace_list::workspaceList(&json!({}), None).ok()
        .and_then(|v| v["workspaces"].as_array().cloned()).unwrap_or_default().iter()
        .any(|w| w["root_path"].as_str().map(|r| r.trim_end_matches(['/', '\\'])) == Some(root));
    if !known && !is_tree { return None; }
    if is_tree { if let Ok(c) = rusqlite::Connection::open(&state_db) { let name = std::path::Path::new(root).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "tree".to_string()); let _ = c.execute("INSERT OR IGNORE INTO workspaces (name, root_path, vcs, is_active, db_dir) VALUES (?1, ?2, 'git', 1, ?3)", rusqlite::params![name, root, state_db.parent().map(|d| d.to_string_lossy().to_string())]); } }   // an older tree database gets its row too
    let c = antiloki_rs::engine_open_db::openDb(&serde_json::json!({ "statePath": state_db.to_string_lossy() }), &serde_json::json!({ "readOnly": false })).ok()?;
    let c = Arc::new(Mutex::new(c)); map.insert(key, c.clone()); Some(c)
}
/// The connection a request runs on: the workspace its header names, else the ordinary pick.
pub fn conn_for(state: &AppState, headers: &HeaderMap) -> Arc<Mutex<rusqlite::Connection>> {
    if let Some(c) = headers.get(WORKSPACE_HEADER).and_then(|v| v.to_str().ok()).and_then(|r| workspace_conn(state, r)) { return c; }
    state.heal_if_gone(); state.pick()
}

/// A route's input template, compiled from the NestJS expression by scripts/compile-input.mjs.
/// Parsed once at router construction, never per request.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct InputTpl {
    #[serde(default)]
    pub base: String, // "body" | "empty"
    #[serde(default)]
    pub defaults: serde_json::Map<String, Value>,
    #[serde(default)]
    pub literals: serde_json::Map<String, Value>,
    #[serde(default)]
    pub project: Vec<Proj>,
    /// Present only for routes whose @Body() is a DTO class; runs BEFORE the input template,
    /// exactly where NestJS's global ValidationPipe sits.
    #[serde(default)]
    pub validate: Option<crate::validate::Validate>,
    /// Manual `throw new BadRequestException(...)` guards in the handler. They run AFTER the DTO
    /// pipe and BEFORE the dispatch, and their envelope carries `message` as a STRING — unlike
    /// class-validator's array. Same status, same keys, different type.
    #[serde(default)]
    pub guards: Vec<Guard>,
    /// Route path parameters folded into the engine input, e.g. `agents/:name` -> `{agent: name}`.
    #[serde(default)]
    pub path_params: Vec<PathParam>,
    /// Some(actor) => signedDispatch: HMAC the call with the driver token and dispatch with
    /// source='driver'. The signature covers the CONSTRUCTED input, not the raw body.
    #[serde(default)]
    pub signed: Option<String>,
    /// Query parameters bound through Nest pipes and folded into the engine input.
    #[serde(default)]
    pub query: Vec<QueryParam>,
    /// Non-default attribution. `None` = dispatchFromHttp's header-derived rules.
    #[serde(default)]
    pub attribution: Option<Attribution>,
    /// The action is chosen by a PATH parameter via a lookup table, e.g.
    /// `POST /driver/card/:intent` with ACTION_BY_INTENT. An unknown key is a 400 listing the
    /// valid ones, in declaration order.
    #[serde(default)]
    pub action_map: Option<ActionMap>,
    /// Guards that inspect PATH parameters rather than the body.
    #[serde(default)]
    pub path_guards: Vec<PathGuard>,
    /// `dispatchViaInterviewer(action, args)` — a TWO-STEP orchestration, not a dispatch. See
    /// run_via_interviewer.
    #[serde(default)]
    pub via_interviewer: Option<String>,
    /// `attachCheckpoints(result)` — after an interviewer turn lands, inline the runtime events
    /// linked to it. See attach_checkpoints.
    #[serde(default)]
    pub post_checkpoints: bool,
    /// `object` guard: `!body.x || typeof body.x !== 'object'`.
    #[serde(default)]
    pub _unused: Option<()>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct ActionMap {
    pub param: String,
    pub keys: Vec<String>,
    pub values: Vec<String>,
    /// "unknown intent '{v}' — must be one of: {keys}"
    pub message_template: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct PathGuard {
    pub param: String,
    #[serde(rename = "type")]
    pub kind: String,
    /// "invalid notification id '{v}'"
    pub message_template: String,
}

/// Guards over path parameters. `Number(id)`-based, matching the handlers:
///   const idNum = Number(id); if (!Number.isFinite(idNum) || idNum <= 0) throw ...
pub fn run_path_guards(guards: &[PathGuard], path: &[(String, String)]) -> Option<Value> {
    for g in guards {
        let raw = path.iter().find(|(k, _)| k == &g.param).map(|(_, v)| v.as_str()).unwrap_or("");
        let tripped = match g.kind.as_str() {
            "number_positive" => {
                let n = raw.trim().parse::<f64>().ok().filter(|f| !f.is_nan());
                !matches!(n, Some(f) if f.is_finite() && f > 0.0)
            }
            _ => false,
        };
        if tripped {
            return Some(json!({
                "message": g.message_template.replace("{v}", raw),
                "error": "Bad Request",
                "statusCode": 400
            }));
        }
    }
    None
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct QueryParam {
    pub key: String,
    pub from: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub min: Option<i64>,
    #[serde(default)]
    pub max: Option<i64>,
    /// `...(phase ? { phase } : {})` — present only when JS-truthy.
    #[serde(default)]
    pub truthy_only: bool,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Attribution {
    /// A literal actor, or the sentinel "session_or_anon" for board's drift/resolve, which
    /// derives `web-fe:<8-char session>` but falls back to `web-fe:anon` — NOT to
    /// `unauthenticated-http` the way dispatchFromHttp does.
    pub actor: String,
    pub source: String,
}

/// Bind query parameters the way Nest's pipes do. Returns Err(nest 400 body) for a value that
/// ParseIntPipe would reject.
///
/// Absence is again the subtle case: a missing `@Query('status') status?: string` is `undefined`
/// in JS, so the key never reaches the engine. Only `int` (via DefaultValuePipe) and
/// `bool_truthy` (a comparison, always yielding a boolean) are present unconditionally.
pub fn bind_query(
    params: &[QueryParam],
    q: &std::collections::HashMap<String, String>,
) -> Result<serde_json::Map<String, Value>, Value> {
    let mut out = serde_json::Map::new();
    for p in params {
        let raw = q.get(&p.from).map(|s| s.as_str());
        match p.kind.as_str() {
            // MEASURED contract, not assumed. Probing the live BE with 9 inputs gave:
            //   accepted -> "" , "abc", "5abc", "1e3", "0", "-5"
            //   rejected -> "1.5", "999999999999999999999"
            // i.e. it is JS `Number(v)` semantics: a value that converts to a NON-integer (or to
            // an integer beyond 2^53) is rejected; a value that does not convert at all falls
            // through to the DefaultValuePipe value. Note "5abc" is accepted, which rules out
            // parseFloat/parseInt as the check. The rejection wording is Nest's, with `message`
            // as a STRING.
            "int" => {
                let reject = || {
                    json!({
                        "message": "Validation failed (numeric string is expected)",
                        "error": "Bad Request",
                        "statusCode": 400
                    })
                };
                let v = match raw {
                    None => p.default.clone(),
                    Some(s) => {
                        // JS Number(): "" is 0, whitespace-only is 0, junk is NaN.
                        // `.filter(!is_nan)` matters: Rust's f64 parser ACCEPTS the literal
                        // "NaN", where JS `Number("NaN")` yields NaN — i.e. "not a number", which
                        // falls through to the default. Without this, ?limit=NaN was a 400 in Rust
                        // and a 200 in the BE.
                        let n = if s.trim().is_empty() {
                            Some(0.0)
                        } else {
                            s.trim().parse::<f64>().ok().filter(|f| !f.is_nan())
                        };
                        match n {
                            // Not a number at all -> the default stands.
                            None => p.default.clone(),
                            Some(f) if f.fract() != 0.0 || f.abs() > 9_007_199_254_740_991.0 => {
                                return Err(reject())
                            }
                            Some(f) => Some(Value::from(f as i64)),
                        }
                    }
                };
                if let Some(v) = v {
                    out.insert(p.key.clone(), v);
                }
            }
            // knowledge.controller's own helper, NOT a Nest pipe:
            //   Math.min(Math.max(parseInt(limit ?? '25', 10) || 25, 1), 100)
            // parseInt (not Number) so "5abc" -> 5, and `|| d` catches NaN AND 0. Never rejects.
            "int_clamped" => {
                let d = p.default.as_ref().and_then(|v| v.as_i64()).unwrap_or(25);
                let parsed = raw
                    .map(|s| {
                        // parseInt: leading numeric prefix, else NaN
                        let t = s.trim();
                        let end = t
                            .char_indices()
                            .position(|(i, c)| !(c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+'))))
                            .unwrap_or(t.len());
                        t[..end].parse::<i64>().unwrap_or(0)
                    })
                    .unwrap_or(d);
                // `|| d` in JS fires on NaN and on 0 alike
                let v = if parsed == 0 { d } else { parsed };
                let v = v.max(p.min.unwrap_or(1)).min(p.max.unwrap_or(100));
                out.insert(p.key.clone(), Value::from(v));
            }
            // `days ? Number(days) : undefined` — omitted unless truthy. Number() of junk is
            // NaN, and JSON.stringify(NaN) is null, so the key arrives as null rather than absent.
            "number_optional" | "number_or_default" => {
                let truthy_raw = raw.filter(|s| !s.is_empty() && *s != "0");
                match truthy_raw {
                    Some(s) => {
                        let n = s.trim().parse::<f64>().ok().filter(|f| !f.is_nan());
                        out.insert(
                            p.key.clone(),
                            match n {
                                Some(f) if f.fract() == 0.0 => Value::from(f as i64),
                                Some(f) => Value::from(f),
                                None => Value::Null, // JSON.stringify(NaN) === "null"
                            },
                        );
                    }
                    None => {
                        if p.kind == "number_or_default" {
                            if let Some(d) = &p.default {
                                out.insert(p.key.clone(), d.clone());
                            }
                        }
                    }
                }
            }
            // `slim === '1' || slim === 'true'` — a comparison, so it is ALWAYS a boolean.
            "bool_truthy" => {
                out.insert(p.key.clone(), Value::Bool(matches!(raw, Some("1") | Some("true"))));
            }
            // `kinds.split(',').map(trim).filter(Boolean)` or undefined when absent/empty.
            "csv" => {
                if let Some(s) = raw.filter(|s| !s.is_empty()) {
                    let list: Vec<Value> = s
                        .split(',')
                        .map(|x| x.trim())
                        .filter(|x| !x.is_empty())
                        .map(|x| Value::String(x.to_string()))
                        .collect();
                    out.insert(p.key.clone(), Value::Array(list));
                }
            }
            _ => match raw {
                Some(s) => {
                    if !(p.truthy_only && s.is_empty()) {
                        out.insert(p.key.clone(), Value::String(s.to_string()));
                    }
                }
                None => {
                    if let Some(d) = &p.default {
                        out.insert(p.key.clone(), d.clone());
                    }
                }
            },
        }
    }
    Ok(out)
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct PathParam {
    pub key: String,
    pub param: String,
    /// "int" => @Param('id', ParseIntPipe). A non-numeric segment is a 400 with Nest's wording,
    /// which W0 captured on GET /api/v1/memory/:id.
    #[serde(default)]
    pub r#type: Option<String>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Guard {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub field: String,
    #[serde(default)]
    pub fields: Vec<GuardField>,
    pub message: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct GuardField {
    pub name: String,
    pub test: String,
}

/// JS truthiness: false, 0, "", null and absent are all falsy.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Some(_) => true,
    }
}

/// Returns the Nest BadRequestException body for the first guard that trips.
/// NOTE `message` is a STRING here. class-validator emits an ARRAY. An FE doing message[0]
/// or message.join() sees different things, so the distinction is preserved exactly.
pub fn run_guards(guards: &[Guard], body: &Value) -> Option<(StatusCode, Value)> {
    for g in guards {
        let tripped = match g.kind.as_str() {
            "truthy" => !truthy(body.get(&g.field)),
            "nonblank" => !body
                .get(&g.field)
                .and_then(|v| v.as_str())
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false),
            "throw_500" => !truthy(body.get(&g.field)),
            // `!body?.position || typeof body.position !== 'object'` — an array IS typeof object
            // in JS, so it passes; only null/absent/primitive trip it.
            "object" => !body.get(&g.field).is_some_and(|v| v.is_object() || v.is_array()),
            "any_of" => !g.fields.iter().any(|f| match f.test.as_str() {
                "not_null" => body.get(&f.name).map(|v| !v.is_null()).unwrap_or(false),
                "nonblank_string" => body
                    .get(&f.name)
                    .and_then(|v| v.as_str())
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false),
                _ => false,
            }),
            _ => false,
        };
        if tripped {
            // A plain `throw new Error(...)` is NOT a BadRequestException: it falls to
            // EngineErrorFilter's unknown-exception branch, which answers 500 with the narrow
            // engine envelope (no skill, no context). enrich/* and review/* rely on this.
            return Some(if g.kind == "throw_500" {
                (StatusCode::INTERNAL_SERVER_ERROR, crate::error::unhandled_body(&g.message))
            } else {
                (
                    StatusCode::BAD_REQUEST,
                    json!({ "message": g.message, "error": "Bad Request", "statusCode": 400 }),
                )
            });
        }
    }
    None
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Proj {
    pub key: String,
    pub from: String,
    #[serde(default)]
    pub coerce: Option<String>,
    #[serde(default)]
    pub default: Option<Value>,
    /// `...(reason ? { reason } : {})` — include only when JS-truthy. Distinct from plain
    /// absence: an empty string IS present but must still be dropped.
    #[serde(default)]
    pub truthy_only: bool,
}

/// Evaluate a template against the request body, matching JS object-construction order:
/// base spread first, then literals, then projections — later writes win.
///
/// The subtle rule is ABSENCE. In JS, `{ k: body.missing }` produces `{k: undefined}`, and
/// JSON.stringify DROPS undefined keys — so the engine never sees `k` at all. Emitting
/// `"k": null` instead would be a different input, and actions that branch on presence would
/// take the other path. Missing projections are therefore omitted, not nulled.
pub fn build_input(tpl: &InputTpl, body: &Value) -> Value {
    build_input_with_path(tpl, body, &[])
}

pub fn build_input_with_path(tpl: &InputTpl, body: &Value, path: &[(String, String)]) -> Value {
    let mut out = serde_json::Map::new();

    if tpl.base == "body" {
        // `body ?? { ... }` fires only when body is null/undefined. Express hands controllers an
        // object for an empty POST, so the default is usually inert — reproduced faithfully
        // rather than "helpfully", so a literal `null` body behaves the same in both engines.
        let source = if body.is_null() { Value::Object(tpl.defaults.clone()) } else { body.clone() };
        if let Some(map) = source.as_object() {
            for (k, v) in map {
                out.insert(k.clone(), v.clone());
            }
        }
    }

    for (k, v) in &tpl.literals {
        out.insert(k.clone(), v.clone());
    }

    for (k, v) in path {
        let Some(pp) = tpl.path_params.iter().find(|pp| &pp.param == k) else { continue };
        let value = if pp.r#type.as_deref() == Some("number_js") {
            // `Number(id)` — NOT ParseIntPipe. Junk becomes NaN, which JSON.stringify renders as
            // null, so the engine receives null rather than a 400.
            match v.trim().parse::<f64>().ok().filter(|f| !f.is_nan()) {
                Some(f) if f.fract() == 0.0 => Value::from(f as i64),
                Some(f) => Value::from(f),
                None => Value::Null,
            }
        } else if pp.r#type.as_deref() == Some("int") {
            match v.trim().parse::<i64>() {
                Ok(n) => Value::from(n),
                // Signalled to the caller as a sentinel rather than returned, because
                // build_input has no error channel; dispatch() checks it before using the input.
                Err(_) => Value::String("__antiloki_bad_int_param__".into()),
            }
        } else {
            Value::String(v.clone())
        };
        out.insert(pp.key.clone(), value);
    }

    for p in &tpl.project {
        let found = body.get(&p.from);
        if p.truthy_only && !truthy(found) {
            continue;
        }
        let value = match p.coerce.as_deref() {
            // !!body.x — always present, always boolean
            Some("bool") => Some(Value::Bool(match found {
                None | Some(Value::Null) => false,
                Some(Value::Bool(b)) => *b,
                Some(Value::String(s)) => !s.is_empty(),
                Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
                Some(_) => true,
            })),
            // body?.x !== false — true unless explicitly false
            Some("not_false") => Some(Value::Bool(found != Some(&Value::Bool(false)))),
            _ => match found {
                Some(Value::Null) | None => p.default.clone(),
                Some(v) => Some(v.clone()),
            },
        };
        // None here means JS would have produced `undefined` — drop the key entirely.
        if let Some(v) = value {
            out.insert(p.key.clone(), v);
        }
    }

    Value::Object(out)
}

/// Port of `dispatchViaInterviewer(action, args)` — used by contracts approve/reject and diffs
/// approve/reject/apply. NOT a dispatch: it drives the interviewer through a plan.
///
///   1. `interviewer.turn` with `{user_text: "BE-routed: <action>", prebuilt_plan:[{action,args,risk_tier:"medium"}]}`
///   2. no `turn_id` -> throw
///   3. `interviewer.dispatch` with `{turn_id, confirmation_text:"go"}`
///   4. take `results[0]`; throw if missing or `!ok`; otherwise return `result.result`
///
/// Every throw here is a PLAIN Error in the BE, so it surfaces as 500 with the narrow engine
/// envelope — not a 400.
fn run_via_interviewer(
    conn: &rusqlite::Connection,
    action: &str,
    args: Value,
) -> Result<Value, String> {
    let ctx = json!({ "actor": "human:via-rest", "source": "chat" });
    let turn = antiloki_rs::engine_action_run::skillRunWithContext(
        &json!({
            "action": "interviewer.turn",
            "input": {
                "user_text": format!("BE-routed: {action}"),
                "prebuilt_plan": [{ "action": action, "args": args, "risk_tier": "medium" }],
            }
        }),
        conn,
        &ctx,
    )?;

    let turn_id = turn.get("turn_id").cloned().filter(|v| !v.is_null());
    let Some(turn_id) = turn_id else {
        return Err(format!("interviewer.turn returned no turn_id (action={action})"));
    };

    let dispatched = antiloki_rs::engine_action_run::skillRunWithContext(
        &json!({
            "action": "interviewer.dispatch",
            "input": { "turn_id": turn_id, "confirmation_text": "go" }
        }),
        conn,
        &ctx,
    )?;

    let result = dispatched.get("results").and_then(|r| r.get(0)).cloned();
    let Some(result) = result else {
        return Err(format!("dispatch returned no results (action={action})"));
    };
    if !result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        let err = result.get("error").and_then(|v| v.as_str()).unwrap_or("");
        return Err(format!("action {action} failed: {err}"));
    }
    Ok(result.get("result").cloned().unwrap_or(Value::Null))
}


/// Port of `attachCheckpoints(result)`.
///
/// After an interviewer turn lands, query the skill/action executions linked to it via
/// `user_goal_id` and inline them. Deliberately NON-FATAL: the BE swallows any failure and
/// returns the raw result, because an older DB may not have the column and the TUI simply does
/// not render the bubble. A 500 here would break a turn that actually succeeded.
pub async fn attach_checkpoints(state: &Arc<AppState>, value: Value) -> Value {
    let Some(turn_id) = value.get("turn_id").cloned().filter(|v| !v.is_null()) else {
        return value;
    };
    let ctx = json!({ "actor": "web-fe", "source": "web-fe" });
    match engine_call(state, "system.chat.checkpoints.list", json!({ "user_goal_id": turn_id }), ctx).await {
        Ok((checkpoints, _)) => {
            let mut out = value;
            if let Some(obj) = out.as_object_mut() {
                obj.insert("checkpoints".into(), checkpoints);
            }
            out
        }
        Err(_) => value,
    }
}

/// The HTTP-derived context, shared by the generated path and the hand-written handlers.
pub fn attribution_ctx(headers: &HeaderMap) -> Value {
    let (actor, source) = attribution(headers);
    let mut ctx = json!({ "actor": actor, "source": source });
    // the workspace the request is ABOUT, when it is not the active one (see `workspace_conn`)
    if let Some(root) = headers.get(WORKSPACE_HEADER).and_then(|v| v.to_str().ok()).filter(|r| !r.is_empty()) { ctx["workspace_root"] = json!(root); }
    ctx
}

/// One engine dispatch on a blocking thread, reporting whether the audit chain grew.
/// Both the generated handler and custom.rs go through this so a skill panic cannot brick the
/// server (the lock is taken past poisoning) and timeline growth is measured identically.
pub async fn engine_call(
    state: &Arc<AppState>,
    action: &str,
    input: Value,
    ctx: Value,
) -> Result<(Value, bool), String> {
    let conn = match ctx.get("workspace_root").and_then(Value::as_str).and_then(|r| workspace_conn(state, r)) { Some(c) => c, None => { state.heal_if_gone(); state.pick() } };
    let call = json!({ "action": action, "input": input });
    match tokio::task::spawn_blocking(move || {
        let guard = conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = last_timeline_id(&guard);
        let out = antiloki_rs::engine_action_run::skillRunWithContext(&call, &guard, &ctx);
        let after = last_timeline_id(&guard);
        out.map(|v| (v, after > before))
    })
    .await
    {
        Ok(r) => {
            // the workspace just changed: rebind before anyone reads the old one again
            if changes_workspace(&action) && r.is_ok() {
                match state.rebind() { Ok(n) => eprintln!("antiloki-serve: workspace activated — {n} connection(s) rebound to the active workspace"), Err(e) => eprintln!("antiloki-serve: rebind after activate failed — {e}") }
            }
            r
        }
        Err(join) => Err(join.to_string()),
    }
}

/// Response mapping shared with the generated path: number normalization, nil-means-empty-body,
/// 201-for-POST, and the typed error envelope.
pub fn finish(
    result: Result<(Value, bool), String>,
    method: &str,
    action: &str,
) -> Response {
    match result {
        Ok((mut value, _grew)) => {
            normalize_numbers(&mut value);
            let status = if method == "POST" { StatusCode::CREATED } else { StatusCode::OK };
            if value.is_null() {
                return status.into_response();
            }
            (status, Json(value)).into_response()
        }
        Err(message) => {
            let err = to_antiloki_error(&message, action);
            (code_to_status(err.code), Json(err.body())).into_response()
        }
    }
}

/// Collapse whole-valued floats to integers, recursively.
///
/// NOT cosmetic — this reproduces a normalization the BE got for free and nobody knew was load
/// bearing. The engine returns f64 for numeric columns, so `system.codebase.history` yields
/// `4633.0`. Through napi that value crossed as a JSON *string*, which the BE JSON.parse'd into a
/// JS Number and JSON.stringify'd back as `4633` — JavaScript has one number type and prints
/// whole floats without a fraction. Serving the engine directly removes that laundering, so the
/// FE would start receiving `4633.0` everywhere it used to read `4633`. Caught by W1 parity on
/// `POST /api/v1/codebase/history`; it would have shipped silently otherwise.
///
/// Bound: 2^53, the point past which f64 stops representing consecutive integers — the same
/// boundary JS's Number.MAX_SAFE_INTEGER draws, so this collapses exactly what JS collapses.
fn normalize_numbers(v: &mut Value) {
    match v {
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if n.as_i64().is_none() && f.is_finite() && f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 {
                    *v = Value::from(f as i64);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(normalize_numbers),
        Value::Object(map) => map.iter_mut().for_each(|(_, val)| normalize_numbers(val)),
        _ => {}
    }
}

/// Port of dispatchFromHttp's attribution rules:
///   X-FE-Session present → actor=`web-fe:<8-char sanitized prefix>`, source=`web-fe`
///   X-FE-Session absent  → actor=`unauthenticated-http`,             source=`unauthenticated`
/// The `|| 'anon'` fallback fires when sanitizing removes every character, not when the header
/// is missing — those are different cases and produce different sources.
fn attribution(headers: &HeaderMap) -> (String, &'static str) {
    let raw = headers.get("x-fe-session").and_then(|v| v.to_str().ok()).unwrap_or("");
    if raw.is_empty() {
        return ("unauthenticated-http".to_string(), "unauthenticated");
    }
    let sanitized: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(8)
        .collect();
    let suffix = if sanitized.is_empty() { "anon".to_string() } else { sanitized };
    (format!("web-fe:{suffix}"), "web-fe")
}

pub async fn dispatch(
    state: Arc<AppState>,
    tpl: Arc<InputTpl>,
    headers: HeaderMap,
    path: Vec<(String, String)>,
    query: std::collections::HashMap<String, String>,
    method: &'static str,
    action: &'static str,
    body: Bytes,
) -> Response {
    // `body ?? {}` in the controllers. An empty body is normal for these routes; a malformed one
    // is NOT this layer's error to invent — Nest would have rejected it at the pipe, which is a
    // W3 concern (see the second error envelope in TAURI-CUTOVER.md).
    let raw_body: Value = if body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(unhandled_body(&format!("invalid JSON body: {e}"))),
                )
                    .into_response()
            }
        }
    };

    // The global ValidationPipe runs before the controller body, so a DTO rejection never
    // reaches the engine and never uses the engine's error envelope.
    if let Some(rules) = &tpl.validate {
        if let Some(nest_error) = crate::validate::validate(rules, &raw_body) {
            return (StatusCode::BAD_REQUEST, Json(nest_error)).into_response();
        }
    }

    // Path guards first: the handlers validate the id BEFORE looking at the body.
    if let Some(err_body) = run_path_guards(&tpl.path_guards, &path) {
        return (StatusCode::BAD_REQUEST, Json(err_body)).into_response();
    }

    // A route whose action is selected by a path parameter (ACTION_BY_INTENT).
    let action: String = match &tpl.action_map {
        Some(m) => {
            let key = path.iter().find(|(k, _)| k == &m.param).map(|(_, v)| v.as_str()).unwrap_or("");
            match m.keys.iter().position(|k| k == key) {
                Some(i) => m.values[i].clone(),
                None => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "message": m.message_template
                                .replace("{v}", key)
                                .replace("{keys}", &m.keys.join(", ")),
                            "error": "Bad Request",
                            "statusCode": 400
                        })),
                    )
                        .into_response()
                }
            }
        }
        None => action.to_string(),
    };
    let action: &str = &action;

    // Manual handler guards run after the pipe, before the dispatch.
    if let Some((status, err_body)) = run_guards(&tpl.guards, &raw_body) {
        return (status, Json(err_body)).into_response();
    }

    let mut input = build_input_with_path(&tpl, &raw_body, &path);
    // ParseIntPipe on a path parameter. Same envelope W0 captured for GET /api/v1/memory/:id.
    if input
        .as_object()
        .is_some_and(|o| o.values().any(|v| v.as_str() == Some("__antiloki_bad_int_param__")))
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "message": "Validation failed (numeric string is expected)",
                "error": "Bad Request",
                "statusCode": 400
            })),
        )
            .into_response();
    }
    match bind_query(&tpl.query, &query) {
        Ok(bound) => {
            if let Some(obj) = input.as_object_mut() {
                for (k, v) in bound {
                    obj.insert(k, v);
                }
            }
        }
        Err(nest_error) => return (StatusCode::BAD_REQUEST, Json(nest_error)).into_response(),
    }
    let input = input;

    // signedDispatch overrides HTTP attribution entirely: the actor is fixed by the route and the
    // source is 'driver', because the call carries a driver-token HMAC.
    let call = json!({ "action": action, "input": input });
    let ctx = match &tpl.signed {
        Some(actor) => {
            let token = match antiloki_rs::audit_driver_token::loadOrCreateDriverToken() {
                Ok(t) => t,
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(unhandled_body(&format!("driver dispatch not initialized - cannot sign {action}: {e}"))),
                    )
                        .into_response()
                }
            };
            let ts = antiloki_rs::clock::now_iso_nodb();
            let signed = antiloki_rs::audit_driver_token::signDriverCall(&json!({
                "action": action,
                "input": input,
                "ts": ts,
                "token": token,
                "actor": actor,
            }));
            json!({
                "actor": actor,
                "source": "driver",
                "driver_hmac": signed.get("hmac").cloned().unwrap_or(Value::Null),
                "driver_ts": signed.get("ts").cloned().unwrap_or(Value::Null),
            })
        }
        None => match &tpl.attribution {
            // Board dispatches with a fixed actor/source; drift/resolve derives the actor from the
            // session header but falls back to `web-fe:anon`, NOT `unauthenticated-http`.
            Some(a) => {
                let actor = if a.actor == "session_or_anon" {
                    let (derived, _) = attribution(&headers);
                    if derived.starts_with("web-fe:") { derived } else { "web-fe:anon".to_string() }
                } else {
                    a.actor.clone()
                };
                json!({ "actor": actor, "source": a.source })
            }
            None => {
                let (actor, source) = attribution(&headers);
                json!({ "actor": actor, "source": source })
            }
        },
    };

    // skillRunWithContext is synchronous and can run for seconds (an action may make AI calls).
    // Holding it on an async worker would stall the runtime, so it goes to a blocking thread.
    // Sampled either side of the dispatch: a higher value means the action mutated AUDITED
    // state, which is the only thing that warrants a change ping. Reads never append, so
    // read-only endpoints stay silent and the FE gets no refetch storm.
    let conn = conn_for(&state, &headers);
    let slug = input
        .get("slug")
        .or_else(|| input.get("card_slug"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let via = tpl.via_interviewer.clone();
    let inner_input = input.clone();
    let result = tokio::task::spawn_blocking(move || {
        let guard = conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = last_timeline_id(&guard);
        let out = match &via {
            Some(inner_action) => run_via_interviewer(&guard, inner_action, inner_input),
            None => antiloki_rs::engine_action_run::skillRunWithContext(&call, &guard, &ctx),
        };
        let after = last_timeline_id(&guard);
        out.map(|v| (v, after > before))
    })
    .await;
    // the workspace just changed: every connection was bound to the old one at boot — rebind them
    // before anyone reads the old workspace's dbs again (the lock above is released by now)
    if changes_workspace(&action) && matches!(&result, Ok(Ok(_))) {
        let st = state.clone();
        match tokio::task::spawn_blocking(move || st.rebind()).await.unwrap_or_else(|e| Err(e.to_string())) {
            Ok(n) => eprintln!("antiloki-serve: workspace activated — {n} connection(s) rebound to the active workspace"),
            Err(e) => eprintln!("antiloki-serve: rebind after activate failed — {e}"),
        }
    }

    let result = match result {
        Ok(Ok((value, grew))) if tpl.post_checkpoints => {
            Ok(Ok((attach_checkpoints(&state, value).await, grew)))
        }
        other => other,
    };

    match result {
        Ok(Ok((mut value, grew))) => {
            normalize_numbers(&mut value);
            if grew {
                // 'cards' rides along: an audited mutation almost always changes the card lists,
                // which keeps the board surfaces live. Verbatim from the BE's emit.
                let mut topics = vec!["actions".to_string(), "cards".to_string()];
                let mut entities = vec![("timeline".to_string(), None, Some("append".to_string()))];
                if let Some(s) = &slug {
                    topics.push(format!("card:{s}"));
                    entities.push(("card".to_string(), Some(s.clone()), Some("update".to_string())));
                }
                state.events.emit(Some(action.to_string()), None, topics, entities);
            }
            // WAKE ONLY ON A MUTATION (2026-09-19): every read the FE polls (session/get, metrics,
            // the brief…) used to wake the driver, and an idle driver tick reads the whole board — on
            // a big workspace that was ~50% of a core, forever. `grew` = the audited timeline grew,
            // i.e. state actually changed; that is when a card may have become actionable.
            if grew {
                if let Some(wake) = state.waker.get() {
                    wake();
                }
            }
            // NestJS answers 201 for POST and 200 for GET. W0 saw 23 routes return 201; a handler
            // that always answered 200 would diverge on every POST in the surface.
            let status = if method == "POST" { StatusCode::CREATED } else { StatusCode::OK };
            // Nest's adapter treats a nil handler result as NO BODY: `return null` sends an empty
            // response, not the four bytes "null". axum's Json(Value::Null) would send "null" and
            // an FE doing `await res.json()` on an empty body throws instead of getting null.
            // Caught on GET /api/v1/card/:slug/review-blueprint, where the action returns null.
            if value.is_null() {
                return status.into_response();
            }
            (status, Json(value)).into_response()
        }
        Ok(Err(message)) => {
            // The interviewer helper throws PLAIN Errors, which the BE's filter renders as 500
            // with the narrow envelope. Only real engine failures get the typed mapping.
            if tpl.via_interviewer.is_some() && !message.starts_with("action-run:") {
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(unhandled_body(&message)))
                    .into_response();
            }
            let err = to_antiloki_error(&message, action);
            (code_to_status(err.code), Json(err.body())).into_response()
        }
        // The blocking task itself panicked — the filter's "unknown exception" branch.
        Err(join_err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(unhandled_body(&join_err.to_string())),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn removing_or_switching_a_workspace_rebinds_the_connections() {
        assert!(super::changes_workspace("system.workspace.activate"));
        assert!(super::changes_workspace("system.workspace.remove"));
        assert!(!super::changes_workspace("system.workspace.add"));   // adding registers; it does not move the active one
    }

    #[test]
    fn a_connection_whose_file_was_trashed_is_noticed() {
        let d = std::env::temp_dir().join(format!("antiloki-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let f = d.join("state.sqlite");
        let c = rusqlite::Connection::open(&f).unwrap();
        c.execute_batch("CREATE TABLE t(x)").unwrap();
        assert!(!super::db_file_gone(&c), "a live file is not gone");
        std::fs::remove_file(&f).unwrap();                       // the folder is moved to the trash: the open connection keeps reading the inode
        assert!(super::db_file_gone(&c), "the unlinked file must be noticed");
        assert!(!super::db_file_gone(&rusqlite::Connection::open_in_memory().unwrap()), "an in-memory db has no file to lose");
        let _ = std::fs::remove_dir_all(&d);
    }

    use super::*;
    use serde_json::json;

    fn tpl(v: Value) -> InputTpl {
        serde_json::from_value(v).expect("template parses")
    }

    #[test]
    fn body_passthrough_is_verbatim() {
        let t = tpl(json!({"base":"body","defaults":{},"literals":{},"project":[]}));
        assert_eq!(build_input(&t, &json!({"a":1,"b":"x"})), json!({"a":1,"b":"x"}));
    }

    #[test]
    fn literals_override_body() {
        // `{ ...(body ?? {}), view: 'today' }` — later keys win, as in JS object construction.
        let t = tpl(json!({"base":"body","defaults":{},"literals":{"view":"today"},"project":[]}));
        assert_eq!(
            build_input(&t, &json!({"view":"ignored","keep":1})),
            json!({"view":"today","keep":1})
        );
    }

    #[test]
    fn empty_base_ignores_body_entirely() {
        // `{}` as the input expression means the body is NOT forwarded.
        let t = tpl(json!({"base":"empty","defaults":{},"literals":{},"project":[]}));
        assert_eq!(build_input(&t, &json!({"a":1})), json!({}));
    }

    #[test]
    fn absent_projection_drops_the_key() {
        // THE subtle rule: JS `{k: body.missing}` is `{k: undefined}`, and JSON.stringify drops
        // undefined keys. Emitting null instead would be a different input to the engine.
        let t = tpl(json!({"base":"empty","defaults":{},"literals":{},
                           "project":[{"key":"contract_slug","from":"contract_slug"}]}));
        assert_eq!(build_input(&t, &json!({})), json!({}));
        assert_eq!(build_input(&t, &json!({"contract_slug":"c"})), json!({"contract_slug":"c"}));
    }

    #[test]
    fn projection_default_fires_only_on_absent_or_null() {
        let t = tpl(json!({"base":"empty","defaults":{},"literals":{},
                           "project":[{"key":"limit","from":"limit","default":5}]}));
        assert_eq!(build_input(&t, &json!({})), json!({"limit":5}));
        assert_eq!(build_input(&t, &json!({"limit":null})), json!({"limit":5}));
        assert_eq!(build_input(&t, &json!({"limit":0})), json!({"limit":0}));
    }

    #[test]
    fn bool_and_not_false_coercions() {
        let t = tpl(json!({"base":"empty","defaults":{},"literals":{},"project":[
            {"key":"force","from":"force","coerce":"bool"},
            {"key":"dry_run","from":"dry_run","coerce":"not_false"}
        ]}));
        // !!undefined === false ; undefined !== false === true
        assert_eq!(build_input(&t, &json!({})), json!({"force":false,"dry_run":true}));
        assert_eq!(
            build_input(&t, &json!({"force":"yes","dry_run":false})),
            json!({"force":true,"dry_run":false})
        );
    }

    #[test]
    fn body_default_fires_only_on_literal_null() {
        // `body ?? { halt: false }` — Express hands controllers `{}` for an empty POST, so the
        // default is inert in practice. Reproduced faithfully rather than "helpfully".
        let t = tpl(json!({"base":"body","defaults":{"halt":false},"literals":{},"project":[]}));
        assert_eq!(build_input(&t, &json!({})), json!({}));
        assert_eq!(build_input(&t, &Value::Null), json!({"halt":false}));
    }

    fn q(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn int_param() -> Vec<QueryParam> {
        serde_json::from_value(json!([{ "key": "limit", "from": "limit", "type": "int", "default": 200 }])).unwrap()
    }

    #[test]
    fn int_pipe_matches_the_measured_be_contract() {
        // Every case below was probed against the live BE; 22/22 matched after this rule.
        let p = int_param();
        // converts to a non-integer, or overflows 2^53 -> 400
        for bad in ["1.5", "-1.5", "1e-3", "999999999999999999999"] {
            assert!(bind_query(&p, &q(&[("limit", bad)])).is_err(), "{bad} should be rejected");
        }
        // does not convert at all -> the DefaultValuePipe value stands
        for fallback in ["abc", "5abc", "0x10", "1,5", "null", "true", "NaN"] {
            let out = bind_query(&p, &q(&[("limit", fallback)])).expect("accepted");
            assert_eq!(out["limit"], json!(200), "{fallback} should fall back to the default");
        }
        // real integers pass through, including the JS-ish spellings
        for (input, want) in [("7", 7), ("007", 7), ("-5", -5), ("0", 0), ("1e3", 1000), ("", 0), ("2.0", 2)] {
            let out = bind_query(&p, &q(&[("limit", input)])).expect("accepted");
            assert_eq!(out["limit"], json!(want), "input {input}");
        }
        // absent -> default
        assert_eq!(bind_query(&p, &q(&[])).unwrap()["limit"], json!(200));
    }

    #[test]
    fn string_query_is_omitted_when_absent_but_bool_is_always_present() {
        let p: Vec<QueryParam> = serde_json::from_value(json!([
            { "key": "status", "from": "status", "type": "string" },
            { "key": "slim", "from": "slim", "type": "bool_truthy" }
        ]))
        .unwrap();
        let out = bind_query(&p, &q(&[])).unwrap();
        assert!(!out.contains_key("status"), "absent string must be dropped, not nulled");
        assert_eq!(out["slim"], json!(false), "`v === '1' || v === 'true'` always yields a boolean");
        assert_eq!(bind_query(&p, &q(&[("slim", "1")])).unwrap()["slim"], json!(true));
        assert_eq!(bind_query(&p, &q(&[("slim", "true")])).unwrap()["slim"], json!(true));
        assert_eq!(bind_query(&p, &q(&[("slim", "yes")])).unwrap()["slim"], json!(false));
    }

    #[test]
    fn csv_query_splits_trims_and_drops_empties() {
        let p: Vec<QueryParam> =
            serde_json::from_value(json!([{ "key": "kinds", "from": "kinds", "type": "csv" }])).unwrap();
        assert!(!bind_query(&p, &q(&[("kinds", "")])).unwrap().contains_key("kinds"));
        assert_eq!(
            bind_query(&p, &q(&[("kinds", "tick, ai ,,x")])).unwrap()["kinds"],
            json!(["tick", "ai", "x"])
        );
    }

    #[test]
    fn whole_floats_collapse_but_real_fractions_survive() {
        // Node laundered these: the engine's f64 4633.0 crossed napi as a JSON string, which the
        // BE JSON.parse'd and JSON.stringify'd back as 4633. Serving the engine directly does not.
        let mut v = json!({"files": 4633.0, "coverage": 0.9, "nested": [{"n": 12.0}]});
        normalize_numbers(&mut v);
        assert_eq!(v.to_string(), r#"{"files":4633,"coverage":0.9,"nested":[{"n":12}]}"#);
    }
}

#[cfg(test)]
mod epoch_tests {
    #[test]
    fn one_serves_switch_does_not_move_anothers_epoch() {
        let (a, b) = ("/tmp/epoch-test-a.db", "/tmp/epoch-test-b.db");
        let before_b = super::workspace_epoch(b);
        super::bump_epoch(a); super::bump_epoch(a);
        assert_eq!(super::workspace_epoch(a), 2);
        assert_eq!(super::workspace_epoch(b), before_b, "another serve in the same process is untouched");
    }

    #[test]
    fn two_workspaces_number_their_new_panes_apart_and_a_workspace_keeps_the_numbers_it_has() {
        use crate::dispatch::{ensure_pane_id_base, pane_id_base};
        assert_eq!(pane_id_base("/p/ministore"), pane_id_base("/p/ministore/"), "the same folder, the same base"); assert_ne!(pane_id_base("/p/ministore"), pane_id_base("/p/e2escratch"));
        assert!(pane_id_base("/p/x") >= 100_000 && pane_id_base("/p/x") < 1_000_000_000, "far from the small numbers panes had, and from a shell pane's");
        let c = rusqlite::Connection::open_in_memory().unwrap(); c.execute_batch("CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT);").unwrap();
        let base = ensure_pane_id_base(&c, "/p/ministore").unwrap(); assert_eq!(base, pane_id_base("/p/ministore"));
        c.execute("INSERT INTO cockpit_panes (name) VALUES ('a')", []).unwrap(); let id: i64 = c.query_row("SELECT id FROM cockpit_panes", [], |r| r.get(0)).unwrap(); assert_eq!(id, base + 1, "the first pane after the base");
        assert_eq!(ensure_pane_id_base(&c, "/p/ministore"), Some(base + 1), "asked again, nothing moves");
        // a workspace that already numbered past its base keeps going; one with old small numbers jumps to the base
        let d = rusqlite::Connection::open_in_memory().unwrap(); d.execute_batch("CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT); INSERT INTO cockpit_panes (name) VALUES ('old'), ('older');").unwrap();
        let b = pane_id_base("/p/e2escratch"); assert_eq!(ensure_pane_id_base(&d, "/p/e2escratch"), Some(b)); d.execute("INSERT INTO cockpit_panes (name) VALUES ('new')", []).unwrap();
        assert_eq!(d.query_row::<i64, _, _>("SELECT MAX(id) FROM cockpit_panes", [], |r| r.get(0)).unwrap(), b + 1); assert_eq!(d.query_row::<i64, _, _>("SELECT COUNT(*) FROM cockpit_panes WHERE id < 10", [], |r| r.get(0)).unwrap(), 2, "the old panes keep their numbers");
        assert_eq!(ensure_pane_id_base(&rusqlite::Connection::open_in_memory().unwrap(), "/p/x"), None, "no panes table: nothing to number");
    }
}

/// A hand-built `AppState` for the other modules' tests: the given connection, no pool.
#[cfg(test)]
pub(crate) fn test_app_state(conn: rusqlite::Connection) -> std::sync::Arc<AppState> {
    std::sync::Arc::new(AppState { waker: std::sync::OnceLock::new(), events: crate::events::EventsHub::new(), conn: std::sync::Arc::new(std::sync::Mutex::new(conn)), pool: vec![], db_path: ":memory:".into() })
}
