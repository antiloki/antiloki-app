//! open-db.rs — twin of skills/engine/open-db.mjs. NAMES ARE CONTRACT: `openDb` (default),
//! `resolveActiveWorkspaceDir` (named export), plus the re-exported path constants
//! (`DEFAULT_STATE_PATH`, …, `DEFAULT_DB_PATH`) modeled here as functions since Rust statics
//! can't read env vars at init time.
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: this is the substrate's own db-FILE opener —
//! it `mkdir`s real directories, opens/creates real sqlite files (or `:memory:`), `ATTACH`es 8
//! sibling databases onto one connection, execs real schema DDL, and calls the already-converted
//! `seedCanonical` / `applyCanonicalLawTriggers` / `ensureCodeSchema` / `ensureCockpitSchema` /
//! `ensureNotebookSchema` / `ensureSourcesSchema` twins. None of that is expressible as a
//! conform-db fixture (real fs + real multi-file sqlite ATTACH is exactly the "db-file open" class
//! the crossing playbook calls out as bespoke-native). Proven below via `#[cfg(test)]` against real
//! temp directories, reusing the `with_home()`-style env-injection convention (here for
//! `ANTILOKI_SUBSTRATE_ROOT`/`ANTILOKI_DATA_DIR`) and the shared `crate::test_env_lock::ENV_LOCK`.
//!
//! Cross-module calls (already-converted twins, reused not reimplemented — playbook rule 11):
//! `crate::engine_open_db_seed::{seedCanonical, applyCanonicalLawTriggers}`,
//! `crate::engine_open_db_schemas::{TIMELINE_SCHEMA, JUDGMENTS_SCHEMA, STATE_SCHEMA, MEMORY_SCHEMA,
//! BRAIN_SCHEMA, LAWS_SCHEMA, KNOWLEDGE_SCHEMA}`, `crate::codebase_code_schema::ensureCodeSchema`,
//! `crate::cockpit_cockpit_schema::ensureCockpitSchema`,
//! `crate::notebooks_notebook_schema::ensureNotebookSchema`,
//! `crate::sources_sources_schema::ensureSourcesSchema`,
//! `crate::engine_registry::resolveActiveProjectDataDir`.
//!
//! PARITY-DEFERRED (disclosed, not ported):
//! 1. **Self-location-derived `SUBSTRATE_ROOT` default.** The JS default (no
//!    `ANTILOKI_SUBSTRATE_ROOT` env override) derives from `import.meta.url` — the *installed
//!    engine's own file location* — which has no Rust equivalent (a compiled binary doesn't carry
//!    "where my source .mjs lives on disk"). The env-var override (which always wins in both
//!    languages) and explicit `input.statePath`/`context.dbPath` are ported faithfully; only the
//!    zero-input, zero-env "figure out the repo root from my own path" fallback falls back here to
//!    `std::env::current_dir()` instead. Every test below sets `ANTILOKI_SUBSTRATE_ROOT` (or passes
//!    an explicit state path) to stay off this fallback, matching how every real caller in the
//!    substrate operates today (never a bare zero-arg `openDb()` outside the self-hosting BE process).
//! 2. **The ~250-line family of "upgrade a DB created by an EARLIER JS schema version" blocks**
//!    (the `molecules`→`blueprints` rename cascade, `swarm_atoms`→`swarm_blocks`, the
//!    `blueprint_json` atoms→blocks forward-conversion, `ai_calls`/`heimdall_ledger`/
//!    `heimdall_evidence`/`heimdall_ai_tags` backfills, `cards`/`contracts`/`agents`/
//!    `interviewer_turns`/`judgment_cases`/`judgment_items`/`court_evidence` defensive
//!    `ADD COLUMN`/`RENAME COLUMN`, and the `idx_cards_parent_slug` existence-gated index) are NOT
//!    ported. Every column/rename those blocks exist to backfill is **already present** in this
//!    crate's own `engine_open_db_schemas` constants (verified: `acceptance_json`, `cloned_from`,
//!    `auto_merge`, `openrouter_session_id`, `build_cmd`, `worktree_path`, `parent_agent`,
//!    `specialization_md`, `applies_to_json`, `is_abstract`, `model_id`, `ai_call_id`, `ai_tier`,
//!    `view_intent`, `offered_widgets_json`, `sources_json`, `snippets_hash`, `defender_accuracy`,
//!    `prosecutor_accuracy`, `judge_accuracy`, `exec_env`, `tick_id`, `blueprint_json` — all
//!    grep-confirmed inside `schemas.rs`'s embedded DDL). So on ANY database this twin creates from
//!    its own `STATE_SCHEMA`/`BRAIN_SCHEMA`/`JUDGMENTS_SCHEMA`, those blocks are — by construction —
//!    unconditional no-ops; there is no "DB predating a migration" for a process that always creates
//!    from the current schema. The one non-schema-column cleanup in that region (deleting stale
//!    `molecule.*`/`system.molecule.get`/`swarm.author.atoms` action rows) IS ported below (a plain
//!    3-statement idempotent `DELETE`, cheap either way).
//! 3. **`context.db = db`** (the JS side-channel that stashes the opened handle onto the caller's
//!    context object for later reuse) has no Rust analogue — callers use the returned `Connection`
//!    directly; nothing observable is lost.
// QUALITY: 80/100 — real fs + real multi-file sqlite ATTACH ported faithfully; two disclosed
// deferrals (self-location default, legacy-DB-upgrade no-ops) documented above.

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[cfg(windows)]
fn homedir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("USERPROFILE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let drive = std::env::var("HOMEDRIVE").ok().filter(|s| !s.is_empty());
    let path = std::env::var("HOMEPATH").ok().filter(|s| !s.is_empty());
    match (drive, path) {
        (Some(d), Some(p)) => Some(PathBuf::from(format!("{d}{p}"))),
        _ => None,
    }
}
#[cfg(not(windows))]
fn homedir() -> Option<PathBuf> {
    std::env::var("HOME").ok().filter(|s| !s.is_empty()).map(PathBuf::from)
}

// roots.mjs: SUBSTRATE_ROOT = resolve(ANTILOKI_SUBSTRATE_ROOT) if set, else derived from the
// engine's own install location (see PARITY-DEFERRED #1 above).
//
// THE FALLBACK IS AN ANCHOR, NOT THE CWD. It used to be `current_dir()`, which is only correct
// when the process happens to start at the repo root — and the BE starts in antiloki-BE, so every
// caller that reached this fallback resolved `.antiloki/db/state.sqlite` under antiloki-BE, a file
// that does not exist. workspace-list.rs hit it first (it opens a RAW second connection to the
// registry by design, bypassing the BE's explicit path) and 500'd on every /api/v1/workspaces/list.
// That was diagnosed once and "fixed" by routing this module's callers through the one resolver
// here — which left the cwd fallback in place, so the bug moved instead of closing.
//
// `CARGO_MANIFEST_DIR` is this crate's own directory, baked at compile time, so its parent is the
// substrate root by construction and does not care where the process was launched from. Same
// anchor consistency-scan.rs and tl-sandbox-create.rs already use. The env var still wins, which
// is how a relocated install or a test fixture retargets it.
fn substrate_root() -> PathBuf {
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .to_path_buf()
}
// roots.mjs: DATA_DIR = resolve(ANTILOKI_DATA_DIR) if set, else join(SUBSTRATE_ROOT, '.antiloki').
fn data_dir() -> PathBuf {
    match std::env::var("ANTILOKI_DATA_DIR") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => substrate_root().join(".antiloki"),
    }
}
fn db_dir() -> PathBuf { data_dir().join("db") }

pub fn default_state_path() -> PathBuf { db_dir().join("state.sqlite") }
pub fn default_memory_path() -> PathBuf { db_dir().join("memory.sqlite") }
pub fn default_brain_path() -> PathBuf { db_dir().join("brain.sqlite") }
pub fn default_laws_path() -> PathBuf { db_dir().join("laws.sqlite") }
pub fn default_knowledge_path() -> PathBuf { db_dir().join("knowledge.sqlite") }
pub fn default_timeline_path() -> PathBuf { db_dir().join("timeline.sqlite") }
pub fn default_judgments_path() -> PathBuf { db_dir().join("judgments.sqlite") }
pub fn default_code_path() -> PathBuf { db_dir().join("code.sqlite") }
pub fn default_notebooks_path() -> PathBuf { db_dir().join("notebooks.sqlite") }
// Back-compat alias for code that imports DEFAULT_DB_PATH.
pub fn default_db_path() -> PathBuf { default_state_path() }

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}
fn bool_field(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}
fn sibling(state_path: &Path, file: &str) -> String {
    state_path.parent().unwrap_or_else(|| Path::new(".")).join(file).to_string_lossy().into_owned()
}
fn is_memory(p: &str) -> bool { p == ":memory:" }

/// Are these the same filesystem location, ignoring how the caller happened to spell it?
///
/// Deliberately NOT `std::fs::canonicalize`: that requires the file to already exist (this is used
/// to decide where to CREATE databases) and on Windows returns `\\?\` verbatim paths, which would
/// reintroduce the same class of mismatch one layer down. Textual normalisation is enough for the
/// question being asked — separator style, redundant `./`, a trailing separator, and on Windows the
/// case-insensitivity the filesystem itself applies.
fn norm_path(p: &str) -> String {
    let mut s = p.replace('\\', "/");
    while let Some(rest) = s.strip_prefix("./") {
        s = rest.to_string();
    }
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    // Windows paths are case-insensitive; POSIX paths are not, so only fold there.
    if cfg!(windows) { s.to_lowercase() } else { s }
}

fn same_path(a: &str, b: &Path) -> bool {
    norm_path(a) == norm_path(&b.to_string_lossy())
}

/// `export function resolveActiveWorkspaceDir(statePath = DEFAULT_STATE_PATH, substrateRoot = SUBSTRATE_ROOT)`.
/// Reads the substrate registry (state.sqlite) for an active workspace with a `db_dir` set, resolved
/// against `substrateRoot`. Fully guarded — any failure (missing file, no active workspace, stale
/// `db_dir` whose `state.sqlite` is gone) resolves to `None`, exactly like the JS `catch { return null }`.
pub fn resolveActiveWorkspaceDir(state_path: &str, substrate_root: &Path) -> Option<PathBuf> {
    if is_memory(state_path) || !Path::new(state_path).exists() {
        return None;
    }
    let reg = Connection::open(state_path).ok()?;
    let db_dir_raw: Option<String> = reg
        .query_row(
            "SELECT db_dir FROM workspaces WHERE is_active = 1 AND db_dir IS NOT NULL LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok();
    let db_dir_raw = db_dir_raw.filter(|s| !s.is_empty())?;
    let dir = substrate_root.join(&db_dir_raw);
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    if !dir.join("state.sqlite").exists() {
        return None;
    }
    Some(dir)
}

struct Paths {
    state: String, memory: String, brain: String, laws: String, knowledge: String,
    timeline: String, judgments: String, code: String, notebooks: String,
}

fn resolve_paths(input: &Value, context: &Value) -> Paths {
    let mut state; let mut memory; let mut brain; let mut laws; let mut knowledge;

    if let Some(s) = input.as_str() {
        state = s.to_string();
        if is_memory(&state) {
            memory = ":memory:".into(); brain = ":memory:".into();
            laws = ":memory:".into(); knowledge = ":memory:".into();
        } else {
            memory = str_field(context, "memoryPath").unwrap_or_else(|| sibling(Path::new(&state), "memory.sqlite"));
            brain  = str_field(context, "brainPath").unwrap_or_else(|| sibling(Path::new(&state), "brain.sqlite"));
            laws   = str_field(context, "lawsPath").unwrap_or_else(|| sibling(Path::new(&state), "laws.sqlite"));
            knowledge = str_field(context, "knowledgePath").unwrap_or_else(|| sibling(Path::new(&state), "knowledge.sqlite"));
        }
    } else if input.is_object() {
        state = str_field(input, "statePath").or_else(|| str_field(input, "path"))
            .or_else(|| str_field(context, "dbPath"))
            .unwrap_or_else(|| default_state_path().to_string_lossy().into_owned());
        let mem_default = || if is_memory(&state) { ":memory:".to_string() } else { sibling(Path::new(&state), "memory.sqlite") };
        let brain_default = || if is_memory(&state) { ":memory:".to_string() } else { sibling(Path::new(&state), "brain.sqlite") };
        let laws_default = || if is_memory(&state) { ":memory:".to_string() } else { sibling(Path::new(&state), "laws.sqlite") };
        let knowledge_default = || if is_memory(&state) { ":memory:".to_string() } else { sibling(Path::new(&state), "knowledge.sqlite") };
        memory = str_field(input, "memoryPath").or_else(|| str_field(context, "memoryPath")).unwrap_or_else(mem_default);
        brain  = str_field(input, "brainPath").or_else(|| str_field(context, "brainPath")).unwrap_or_else(brain_default);
        laws   = str_field(input, "lawsPath").or_else(|| str_field(context, "lawsPath")).unwrap_or_else(laws_default);
        knowledge = str_field(input, "knowledgePath").or_else(|| str_field(context, "knowledgePath")).unwrap_or_else(knowledge_default);
    } else {
        state = default_state_path().to_string_lossy().into_owned();
        memory = default_memory_path().to_string_lossy().into_owned();
        brain = default_brain_path().to_string_lossy().into_owned();
        laws = default_laws_path().to_string_lossy().into_owned();
        knowledge = default_knowledge_path().to_string_lossy().into_owned();
    }

    // FEAT-WORKSPACE-CRUD-API: a plain default open (no explicit paths, not :memory:) with an
    // active workspace's own DB set swaps all five paths to that set.
    //
    // COMPARED AS PATHS, NOT AS STRINGS. This was five raw `==` checks, which made the swap depend
    // on the caller spelling a path byte-identically to the way `default_state_path()` happens to
    // build it. On Windows those differ constantly and invisibly: the defaults are built with
    // `PathBuf::join` (backslashes), while callers pass paths from env vars, JSON and config that
    // routinely use forward slashes or a different drive-letter case. One mismatch and the swap is
    // silently skipped — the engine then reads and writes the DEFAULT substrate while the caller
    // believes it is in the active workspace, with no error anywhere. Same set of tables, wrong
    // database; you notice because your atlas is mysteriously empty.
    //
    // `same_path` normalises separators and (on Windows only) case, so the comparison asks what it
    // meant to ask all along: is this the default location? — not: are these two strings equal?
    let is_default_open = same_path(&state, &default_state_path())
        && same_path(&memory, &default_memory_path())
        && same_path(&brain, &default_brain_path())
        && same_path(&laws, &default_laws_path())
        && same_path(&knowledge, &default_knowledge_path());
    if is_default_open {
        // JS: `resolveActiveProjectDataDir() || resolveActiveWorkspaceDir()` — the machine
        // registry (~/.antiloki/registry.sqlite) wins over the legacy self-host resolver.
        let ws_dir = crate::engine_registry::resolveActiveProjectDataDir(&crate::engine_registry::registry_path())
            .or_else(|| resolveActiveWorkspaceDir(&state, &substrate_root()));
        if let Some(ws_dir) = ws_dir {
            state = ws_dir.join("state.sqlite").to_string_lossy().into_owned();
            memory = ws_dir.join("memory.sqlite").to_string_lossy().into_owned();
            brain = ws_dir.join("brain.sqlite").to_string_lossy().into_owned();
            laws = ws_dir.join("laws.sqlite").to_string_lossy().into_owned();
            knowledge = ws_dir.join("knowledge.sqlite").to_string_lossy().into_owned();
        }
    }

    let sib_or = |field: &str, default_file: &str| -> String {
        str_field(input, field).or_else(|| str_field(context, field))
            .unwrap_or_else(|| if is_memory(&state) { ":memory:".to_string() } else { sibling(Path::new(&state), default_file) })
    };
    let timeline = sib_or("timelinePath", "timeline.sqlite");
    let judgments = sib_or("judgmentsPath", "judgments.sqlite");
    let code = sib_or("codePath", "code.sqlite");
    let notebooks = sib_or("notebooksPath", "notebooks.sqlite");

    Paths { state, memory, brain, laws, knowledge, timeline, judgments, code, notebooks }
}

fn mkdir_for(p: &str) {
    if !is_memory(p) {
        if let Some(parent) = Path::new(p).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
}
fn esc(p: &str) -> String { p.replace('\'', "''") }

fn attach_write(db: &Connection, path: &str, alias: &str) -> Result<(), String> {
    db.execute_batch(&format!("ATTACH DATABASE '{}' AS {}", esc(path), alias))
        .map_err(|e| e.to_string())?;
    if !is_memory(path) {
        let _ = db.execute_batch(&format!("PRAGMA {alias}.journal_mode = WAL"));
        let _ = db.execute_batch(&format!("PRAGMA {alias}.synchronous = NORMAL"));
    }
    Ok(())
}

/// `export default function openDb(input = {}, context = {})`. Returns the opened+attached+seeded
/// connection (or, on `readOnly`, the opened+attached connection with schema/seed work skipped —
/// see the JS "read-only fast path" comment).
pub fn openDb(input: &Value, context: &Value) -> Result<Connection, String> {
    let paths = resolve_paths(input, context);
    for p in [&paths.state, &paths.memory, &paths.brain, &paths.laws, &paths.knowledge,
              &paths.timeline, &paths.judgments, &paths.code, &paths.notebooks] {
        mkdir_for(p);
    }

    let read_only = bool_field(input, "readOnly") || bool_field(context, "readOnly");
    let db = if read_only {
        Connection::open_with_flags(&paths.state, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?
    } else {
        Connection::open(&paths.state).map_err(|e| e.to_string())?
    };
    let busy_timeout_ms: i64 = std::env::var("ANTILOKI_BUSY_TIMEOUT_MS").ok()
        .and_then(|v| v.parse().ok()).unwrap_or(8000);
    let _ = db.execute_batch(&format!("PRAGMA busy_timeout = {busy_timeout_ms}"));

    if read_only {
        // PER-ATTACH ISOLATION: one bad/missing sibling only costs its own alias.
        let attach_ro = |path: &str, alias: &str| {
            if is_memory(path) { return; }
            let _ = db.execute_batch(&format!("ATTACH DATABASE '{}' AS {}", esc(path), alias));
        };
        attach_ro(&paths.memory, "memory");
        attach_ro(&paths.brain, "brain");
        attach_ro(&paths.laws, "laws");
        attach_ro(&paths.knowledge, "knowledge");
        attach_ro(&paths.timeline, "timeline");
        attach_ro(&paths.judgments, "judgments");
        attach_ro(&paths.code, "code");
        attach_ro(&paths.notebooks, "nb");

        let ro_cache_kb: i64 = std::env::var("ANTILOKI_RO_CACHE_KB").ok()
            .and_then(|v| v.parse().ok()).unwrap_or(262144);
        let ro_mmap: i64 = std::env::var("ANTILOKI_RO_MMAP_BYTES").ok()
            .and_then(|v| v.parse().ok()).unwrap_or(1073741824);
        for alias in ["main", "memory", "brain", "laws", "knowledge", "timeline", "judgments", "code", "nb"] {
            let px = if alias == "main" { String::new() } else { format!("{alias}.") };
            let _ = db.execute_batch(&format!("PRAGMA {px}cache_size = -{ro_cache_kb}"));
            let _ = db.execute_batch(&format!("PRAGMA {px}mmap_size = {ro_mmap}"));
        }
        return Ok(db);
    }

    if !is_memory(&paths.state) {
        let _ = db.execute_batch("PRAGMA journal_mode = WAL");
        let _ = db.execute_batch("PRAGMA synchronous = NORMAL");
        let wal_ckpt: i64 = std::env::var("ANTILOKI_WAL_AUTOCHECKPOINT").ok()
            .and_then(|v| v.parse().ok()).unwrap_or(2000);
        let _ = db.execute_batch(&format!("PRAGMA wal_autocheckpoint = {wal_ckpt}"));
    }
    db.execute_batch(crate::engine_open_db_schemas::STATE_SCHEMA.as_str().unwrap())
        .map_err(|e| e.to_string())?;

    attach_write(&db, &paths.memory, "memory")?;
    db.execute_batch(crate::engine_open_db_schemas::MEMORY_SCHEMA.as_str().unwrap())
        .map_err(|e| e.to_string())?;
    // FEAT-MEMORY-AGENT-SCOPED: index on agent_slug (present in this twin's MEMORY_SCHEMA already,
    // guarded exactly like the JS in case a future schema drop removes it).
    let has_agent_slug: bool = db.prepare("PRAGMA memory.table_info(memories)").ok()
        .and_then(|mut s| s.query_map([], |r| r.get::<_, String>(1)).ok()
            .map(|it| it.flatten().any(|n| n == "agent_slug")))
        .unwrap_or(false);
    if has_agent_slug {
        let _ = db.execute_batch(
            "CREATE INDEX IF NOT EXISTS memory.idx_memories_agent ON memories(agent_slug) WHERE agent_slug IS NOT NULL");
    }

    attach_write(&db, &paths.brain, "brain")?;
    db.execute_batch(crate::engine_open_db_schemas::BRAIN_SCHEMA.as_str().unwrap())
        .map_err(|e| e.to_string())?;

    attach_write(&db, &paths.laws, "laws")?;
    db.execute_batch(crate::engine_open_db_schemas::LAWS_SCHEMA.as_str().unwrap())
        .map_err(|e| e.to_string())?;

    attach_write(&db, &paths.knowledge, "knowledge")?;
    db.execute_batch(crate::engine_open_db_schemas::KNOWLEDGE_SCHEMA.as_str().unwrap())
        .map_err(|e| e.to_string())?;

    // FEAT-TIMELINE-RELOCATION: create TIMELINE_SCHEMA only if the legacy in-state copy is absent
    // (so an unqualified reference never binds ambiguously to an empty new 'timeline' copy).
    attach_write(&db, &paths.timeline, "timeline")?;
    if !is_memory(&paths.timeline) {
        let wal_ckpt: i64 = std::env::var("ANTILOKI_WAL_AUTOCHECKPOINT").ok()
            .and_then(|v| v.parse().ok()).unwrap_or(2000);
        let _ = db.execute_batch(&format!("PRAGMA timeline.wal_autocheckpoint = {wal_ckpt}"));
    }
    let main_has_timeline: bool = db.query_row(
        "SELECT 1 FROM main.sqlite_master WHERE type='table' AND name='sacred_timeline'", [],
        |_| Ok(true)).unwrap_or(false);
    if !main_has_timeline {
        db.execute_batch(crate::engine_open_db_schemas::TIMELINE_SCHEMA.as_str().unwrap())
            .map_err(|e| e.to_string())?;
    }

    // EVENT-SOURCED-AUDIT Phase 2: audit_stage always ensured (never lands via TIMELINE_SCHEMA).
    let _ = db.execute_batch(
        "CREATE TABLE IF NOT EXISTS timeline.audit_stage (
            id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT, tick_id INTEGER, phase TEXT,
            ts TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), actor TEXT NOT NULL, operation TEXT NOT NULL,
            entity_type TEXT NOT NULL, entity_id TEXT NOT NULL,
            before_json TEXT, after_json TEXT, payload TEXT, exec_env TEXT, user_goal_id INTEGER
        )");
    let _ = db.execute_batch(
        "CREATE INDEX IF NOT EXISTS timeline.idx_audit_stage_session ON audit_stage(session_id, id)");

    attach_write(&db, &paths.judgments, "judgments")?;
    db.execute_batch(crate::engine_open_db_schemas::JUDGMENTS_SCHEMA.as_str().unwrap())
        .map_err(|e| e.to_string())?;

    attach_write(&db, &paths.code, "code")?;
    crate::codebase_code_schema::ensureCodeSchema(&db, &Value::String("code.".into()));

    // ONE-TIME BACKFILL: code_onboard_jobs moved from the `code` schema to `main` (see the table's
    // note in schemas.rs — in `code` it was invisible to the poller during a staged rebuild). Rows
    // written before the move are stranded in code.sqlite, and the FE polls a job_id it remembers
    // across restarts: after the move that id resolved to nothing, so the poll returned NOT_FOUND
    // forever and hammered the log rather than ever settling. Copying them forward makes an old id
    // resolve to its real outcome.
    //
    // INSERT OR IGNORE, so the copy is idempotent and `main` always wins — a row that already
    // exists here is the live one and is never overwritten by the legacy copy. Both tables carry
    // the same columns, so this is a verbatim transfer; nothing is invented. Fully swallowed: a
    // missing legacy table (a fresh substrate) is the normal case, not an error.
    let _ = db.execute_batch(
        "INSERT OR IGNORE INTO main.code_onboard_jobs \
           (job_id, root, phase, status, profile_json, error, detail, created_at, updated_at) \
         SELECT job_id, root, phase, status, profile_json, error, detail, created_at, updated_at \
         FROM code.code_onboard_jobs",
    );
    crate::cockpit_cockpit_schema::ensureCockpitSchema(&db, &Value::String(String::new())); // main state DB

    attach_write(&db, &paths.notebooks, "nb")?;
    crate::notebooks_notebook_schema::ensureNotebookSchema(&db, &Value::String("nb.".into()));
    crate::sources_sources_schema::ensureSourcesSchema(&db, &Value::String("knowledge.".into()));

    let _ = db.execute_batch("DROP VIEW IF EXISTS main.timeline_with_commits");

    crate::engine_open_db_seed::seedCanonical(&db);
    // EPIC-BLOCKS-BINDINGS-CUTOVER: prune stale rows for the renamed molecule.* actions (the FK
    // from agent_actions/agent_stage_actions blocks a bare DELETE, so clear dependents first).
    let stale = "action_name LIKE 'molecule.%' OR action_name = 'system.molecule.get' OR action_name = 'swarm.author.atoms'";
    let _ = db.execute_batch(&format!("DELETE FROM agent_actions WHERE {stale}"));
    let _ = db.execute_batch(&format!("DELETE FROM agent_stage_actions WHERE {stale}"));
    let _ = db.execute_batch(
        "DELETE FROM actions WHERE name LIKE 'molecule.%' OR name = 'system.molecule.get' OR name = 'swarm.author.atoms'");

    crate::engine_open_db_seed::applyCanonicalLawTriggers(&db);

    // DEFENSIVE COLUMNS (2026-09-19): a DB created before a column joined the DDL gets it here —
    // `CREATE TABLE IF NOT EXISTS` never alters an existing table. `outcome_class` on ai_calls was
    // read by six skills (Heimdall's prompt list 500'd: "no such column") and declared by none.
    ensure_column(&db, "brain", "ai_calls", "outcome_class", "TEXT");

    Ok(db)
}

/// `ALTER TABLE <schema>.<table> ADD COLUMN <col> <ddl>` when the column is missing. Idempotent, quiet.
pub fn ensure_column(db: &Connection, schema: &str, table: &str, col: &str, ddl: &str) {
    let has = db
        .prepare(&format!("SELECT 1 FROM pragma_table_info('{table}', '{schema}') WHERE name = ?1"))
        .and_then(|mut s| s.exists([col]))
        .unwrap_or(true); // an unreadable pragma → do nothing rather than fail the open
    if !has { let _ = db.execute_batch(&format!("ALTER TABLE {schema}.{table} ADD COLUMN {col} {ddl}")); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    #[test]
    fn ensure_column_adds_a_missing_column_once_and_leaves_a_present_one_alone() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("ATTACH DATABASE ':memory:' AS brain; CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY);").unwrap();
        ensure_column(&db, "brain", "ai_calls", "outcome_class", "TEXT");
        ensure_column(&db, "brain", "ai_calls", "outcome_class", "TEXT"); // idempotent
        let n: i64 = db.query_row("SELECT COUNT(*) FROM pragma_table_info('ai_calls', 'brain') WHERE name = 'outcome_class'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        db.execute_batch("INSERT INTO brain.ai_calls (id, outcome_class) VALUES (1, 'clean')").unwrap();
    }

    // Also pins ANTILOKI_HOME to the same isolated root: `resolveActiveProjectDataDir` (checked
    // BEFORE the workspace-dir fallback below) reads `~/.antiloki/registry.sqlite` by default —
    // without this, a real machine registry.sqlite could resolve first and swap the default open
    // to a REAL project dir instead of exercising the intended fallback path.
    fn with_substrate_root<T>(root: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", root);
            std::env::set_var("ANTILOKI_HOME", root);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT");
            std::env::remove_var("ANTILOKI_HOME");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-open-db-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn table_exists(db: &Connection, schema: &str, name: &str) -> bool {
        db.query_row(
            &format!("SELECT 1 FROM {schema}.sqlite_master WHERE type='table' AND name=?1"),
            rusqlite::params![name],
            |_| Ok(true),
        ).unwrap_or(false)
    }

    #[test]
    fn same_path_ignores_separator_style_and_windows_case() {
        // The exact shape that broke: a caller passing forward slashes while the default is built
        // with `PathBuf::join`. As a raw `==` this is false, the workspace swap silently does not
        // happen, and the engine works on the default substrate while believing it is in the
        // active workspace.
        let dflt = default_state_path();
        let forward = dflt.to_string_lossy().replace('\\', "/");
        assert!(same_path(&forward, &dflt), "separator style must not decide which database opens");
        assert!(same_path(&format!("{forward}/"), &dflt) || !forward.ends_with('/'), "trailing separator tolerated");
        if cfg!(windows) {
            assert!(same_path(&forward.to_uppercase(), &dflt), "windows paths are case-insensitive");
        }
        // Still discriminating: a genuinely different location must NOT compare equal.
        assert!(!same_path("C:/somewhere/else/state.sqlite", &dflt));
        assert!(!same_path(":memory:", &dflt));
    }

    #[test]
    fn open_db_swaps_to_the_active_workspace_even_when_the_path_is_spelled_differently() {
        // THE BUG THIS PINS: `is_default_open` used five raw string `==` comparisons, so passing
        // the default state path with forward slashes skipped the swap and opened the DEFAULT
        // substrate — no error, just the wrong database.
        let root = temp_dir("swap-spelling");
        let default_db_dir = root.join(".antiloki").join("db");
        fs::create_dir_all(&default_db_dir).unwrap();
        let ws_dir = root.join(".antiloki").join("workspaces").join("bravo");
        fs::create_dir_all(&ws_dir).unwrap();
        fs::write(ws_dir.join("state.sqlite"), b"").unwrap(); // presence-only check

        with_substrate_root(root.to_string_lossy().as_ref(), || {
            let default_state = default_state_path();
            let conn = Connection::open(&default_state).unwrap();
            conn.execute_batch(
                "CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT, db_dir TEXT, is_active INTEGER)"
            ).unwrap();
            let rel = format!(".antiloki{}workspaces{}bravo", std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR);
            conn.execute("INSERT INTO workspaces (name, db_dir, is_active) VALUES ('bravo', ?1, 1)",
                         rusqlite::params![rel]).unwrap();
            drop(conn);

            // Spelled with forward slashes — the way an env var or JSON config carries it.
            let forward = default_state.to_string_lossy().replace('\\', "/");
            let db = openDb(&json!(forward), &json!({})).unwrap();
            drop(db);
            assert!(ws_dir.join("cards.sqlite").exists() || table_count(&ws_dir.join("state.sqlite")) > 0,
                    "the swap must fire on a differently-spelled default path, not just a byte-identical one");
        });
        let _ = fs::remove_dir_all(&root);
    }

    fn table_count(p: &Path) -> i64 {
        Connection::open(p).ok()
            .and_then(|c| c.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table'", [], |r| r.get(0)).ok())
            .unwrap_or(0)
    }

    #[test]
    fn open_db_backfills_legacy_onboard_jobs_from_the_code_schema() {
        // THE REGRESSION: code_onboard_jobs moved code → main. The FE remembers a job_id across
        // restarts, so after the move it polled an id that resolved to nothing and got NOT_FOUND on
        // a loop forever instead of ever settling. Opening must carry the legacy rows forward.
        let root = temp_dir("onboard-backfill");
        let db_dir = root.join(".antiloki").join("db");
        fs::create_dir_all(&db_dir).unwrap();
        let state_path = db_dir.join("state.sqlite");
        let code_path = db_dir.join("code.sqlite");

        // A legacy row, written where the table used to live — and NOT in state.
        {
            let code = Connection::open(&code_path).unwrap();
            crate::codebase_code_schema::ensureCodeSchema(&code, &Value::String(String::new()));
            code.execute(
                "INSERT INTO code_onboard_jobs (job_id, root, phase, status, created_at, updated_at) \
                 VALUES ('legacy-1', 'C:/repo', 'done', 'done', '2026-01-01 00:00:00', '2026-01-01 00:00:00')",
                [],
            ).unwrap();
        }

        let db = openDb(&json!(state_path.to_string_lossy().to_string()), &json!({})).unwrap();
        let phase: String = db
            .query_row("SELECT phase FROM main.code_onboard_jobs WHERE job_id = 'legacy-1'", [], |r| r.get(0))
            .expect("the legacy row must be readable from main after opening");
        assert_eq!(phase, "done", "carried forward verbatim, not re-derived");

        // Idempotent, and main always wins: a second open must not clobber a live row that has
        // since moved on with the stale legacy copy of it.
        db.execute("UPDATE main.code_onboard_jobs SET phase = 'ingest' WHERE job_id = 'legacy-1'", []).unwrap();
        drop(db);
        let db2 = openDb(&json!(state_path.to_string_lossy().to_string()), &json!({})).unwrap();
        let phase2: String = db2
            .query_row("SELECT phase FROM main.code_onboard_jobs WHERE job_id = 'legacy-1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(phase2, "ingest", "INSERT OR IGNORE — the live row survives a re-open");
        drop(db2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn open_db_memory_attaches_all_eight_siblings_and_creates_full_schema() {
        let db = openDb(&json!(":memory:"), &json!({})).unwrap();
        assert!(table_exists(&db, "main", "cards"), "state schema");
        assert!(table_exists(&db, "main", "agents"), "state schema");
        assert!(table_exists(&db, "memory", "memories"), "memory attached + schema");
        assert!(table_exists(&db, "brain", "ai_calls"), "brain attached + schema");
        assert!(table_exists(&db, "laws", "laws"), "laws attached + schema");
        assert!(table_exists(&db, "knowledge", "documents") || table_exists(&db, "knowledge", "sources"),
            "knowledge attached + schema");
        assert!(table_exists(&db, "timeline", "sacred_timeline"), "timeline attached + schema");
        assert!(table_exists(&db, "timeline", "audit_stage"), "audit_stage always ensured");
        assert!(table_exists(&db, "judgments", "judgment_cases"), "judgments attached + schema");
        assert!(table_exists(&db, "code", "code_nodes"), "code attached + ensureCodeSchema");
        assert!(table_exists(&db, "nb", "notebooks") || table_exists(&db, "nb", "notebook_items"),
            "notebooks attached + ensureNotebookSchema");
    }

    #[test]
    fn open_db_memory_runs_seed_canonical_and_law_triggers() {
        let db = openDb(&json!(":memory:"), &json!({})).unwrap();
        let n_laws: i64 = db.query_row("SELECT COUNT(*) FROM laws.laws", [], |r| r.get(0)).unwrap();
        assert!(n_laws > 0, "seedCanonical seeded laws.laws");
        let n_agents: i64 = db.query_row("SELECT COUNT(*) FROM agents", [], |r| r.get(0)).unwrap();
        assert!(n_agents > 0, "seedCanonical seeded agents");
    }

    #[test]
    fn open_db_explicit_state_path_creates_sibling_files_on_real_disk() {
        let dir = temp_dir("siblings");
        let state_path = dir.join("state.sqlite");
        let input = json!({ "statePath": state_path.to_string_lossy() });
        let db = openDb(&input, &json!({})).unwrap();
        let mode: String = db.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode, "wal");
        drop(db);
        for f in ["state.sqlite", "memory.sqlite", "brain.sqlite", "laws.sqlite", "knowledge.sqlite",
                  "timeline.sqlite", "judgments.sqlite", "code.sqlite", "notebooks.sqlite"] {
            assert!(dir.join(f).exists(), "{f} should exist on disk");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_db_string_input_memory_puts_every_sibling_in_memory() {
        // String-input branch (not the object branch) — distinct code path per the JS source.
        let db = openDb(&json!(":memory:"), &json!({})).unwrap();
        assert!(table_exists(&db, "brain", "ai_calls"));
    }

    #[test]
    fn open_db_read_only_fast_path_reads_but_refuses_writes() {
        let dir = temp_dir("readonly");
        let state_path = dir.join("state.sqlite");
        {
            let db = openDb(&json!({ "statePath": state_path.to_string_lossy() }), &json!({})).unwrap();
            drop(db);
        }
        let ro = openDb(
            &json!({ "statePath": state_path.to_string_lossy(), "readOnly": true }),
            &json!({}),
        ).unwrap();
        let n: i64 = ro.query_row("SELECT COUNT(*) FROM cards", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        let n_laws: i64 = ro.query_row("SELECT COUNT(*) FROM laws.laws", [], |r| r.get(0)).unwrap();
        assert!(n_laws > 0, "read-only connection still sees the seeded laws written before reopen");
        let write_err = ro.execute("INSERT INTO cards (slug, title) VALUES ('x','y')", []);
        assert!(write_err.is_err(), "read-only connection must refuse writes");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_db_read_only_per_alias_isolation_survives_a_missing_sibling() {
        let dir = temp_dir("ro-isolation");
        let state_path = dir.join("state.sqlite");
        {
            let db = openDb(&json!({ "statePath": state_path.to_string_lossy() }), &json!({})).unwrap();
            drop(db);
        }
        // Delete the memory sibling (+ WAL sidecars) to simulate a locked/missing sibling.
        for suffix in ["", "-wal", "-shm"] {
            let _ = fs::remove_file(dir.join(format!("memory.sqlite{suffix}")));
        }
        let ro = openDb(
            &json!({ "statePath": state_path.to_string_lossy(), "readOnly": true }),
            &json!({}),
        ).unwrap();
        // brain (a different, still-present sibling) is still queryable.
        let n_laws: i64 = ro.query_row("SELECT COUNT(*) FROM laws.laws", [], |r| r.get(0)).unwrap();
        assert!(n_laws > 0, "an unrelated present sibling still attaches + queries fine");
    }

    #[test]
    fn resolve_active_workspace_dir_none_when_state_missing_or_no_active_workspace() {
        let dir = temp_dir("resolve-none");
        let missing = dir.join("nope.sqlite");
        assert!(resolveActiveWorkspaceDir(&missing.to_string_lossy(), &dir).is_none());

        let state_path = dir.join("state.sqlite");
        let conn = Connection::open(&state_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT, db_dir TEXT, is_active INTEGER)"
        ).unwrap();
        drop(conn);
        assert!(resolveActiveWorkspaceDir(&state_path.to_string_lossy(), &dir).is_none(), "no active row");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_active_workspace_dir_returns_dir_when_active_and_state_sqlite_present() {
        let dir = temp_dir("resolve-some");
        let state_path = dir.join("state.sqlite");
        let ws_dir = dir.join("workspaces").join("alfa");
        fs::create_dir_all(&ws_dir).unwrap();
        fs::write(ws_dir.join("state.sqlite"), b"").unwrap(); // presence-only check, not opened

        let conn = Connection::open(&state_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT, db_dir TEXT, is_active INTEGER)"
        ).unwrap();
        conn.execute(
            "INSERT INTO workspaces (name, db_dir, is_active) VALUES ('alfa', 'workspaces/alfa', 1)", [],
        ).unwrap();
        drop(conn);

        let resolved = resolveActiveWorkspaceDir(&state_path.to_string_lossy(), &dir).unwrap();
        assert_eq!(std::fs::canonicalize(&resolved).unwrap(), std::fs::canonicalize(&ws_dir).unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_db_default_open_swaps_to_active_workspace_dir_when_present() {
        let root = temp_dir("default-swap-root");
        let default_db_dir = root.join(".antiloki").join("db");
        fs::create_dir_all(&default_db_dir).unwrap();
        let ws_dir = root.join(".antiloki").join("workspaces").join("alfa");
        fs::create_dir_all(&ws_dir).unwrap();
        // resolveActiveWorkspaceDir guards against a stale `db_dir` whose state.sqlite is gone
        // (JS: `if (!existsSync(join(dir, 'state.sqlite'))) return null;`) — the workspace's own
        // state.sqlite must already exist for the swap to take effect.
        Connection::open(ws_dir.join("state.sqlite")).unwrap();

        with_substrate_root(root.to_string_lossy().as_ref(), || {
            // Seed the DEFAULT state.sqlite (what a bare openDb() would open) with an active
            // workspace row pointing at ws_dir (relative to substrate_root).
            let default_state = default_state_path();
            let conn = Connection::open(&default_state).unwrap();
            conn.execute_batch(
                "CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT, db_dir TEXT, is_active INTEGER)"
            ).unwrap();
            let rel = format!(".antiloki{}workspaces{}alfa", std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR);
            conn.execute(
                "INSERT INTO workspaces (name, db_dir, is_active) VALUES ('alfa', ?1, 1)",
                rusqlite::params![rel],
            ).unwrap();
            drop(conn);

            let db = openDb(&json!({}), &json!({})).unwrap();
            assert!(table_exists(&db, "main", "cards"), "opened the swapped-to workspace state db");
            drop(db);
            assert!(ws_dir.join("state.sqlite").exists(), "wrote into the ACTIVE workspace dir, not the default");
        });
        let _ = fs::remove_dir_all(&root);
    }
}
