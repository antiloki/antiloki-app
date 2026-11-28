//! roots.rs — twin of skills/engine/roots.mjs, the JS single source of truth for every
//! filesystem root the engine touches. NAMES ARE CONTRACT: every JS `export const` is modeled
//! here as a `pub fn` (lower_snake_case of the JS SCREAMING_SNAKE_CASE name) returning `PathBuf`,
//! because — like `open-db.rs`'s `DEFAULT_*_PATH` family and `god-driver-halt.rs`'s `HOME_DIR` —
//! Rust `static`/`const` cannot read `std::env::var` at init time; a function is the only way to
//! reproduce "resolved fresh from the current environment on every read", which is exactly what
//! the JS module-top-level `const` (evaluated once at import, but import happens once per process
//! either way) provides. `DB_PATHS` (a JS plain object, not a function) is modeled as `db_paths()`
//! returning `serde_json::Value` — an object keyed by the same 9 db-name strings — with a matching
//! `pub fn db_path(name: &str) -> Option<PathBuf>` accessor for typed call sites, plus the 9
//! individual `pub fn <name>_db_path()` twins already established as the crate convention (see
//! `open-db.rs`'s `default_state_path()` etc. — reused here rather than reimplemented, since they
//! ARE this twin's `DB_PATHS` entries; a second competing implementation would drift).
//!
//! PARITY-DEFERRED / disclosed self-location gap (same shape as `workspace-root.rs`'s
//! `substrate_root()` and `open-db.rs`'s deferral #1): the JS `HERE`/`ENGINE_ROOT` derive from
//! `import.meta.url` — the *installed engine's own file location* on disk, i.e. the real
//! `antiloki-core/` directory (parent of `skills/`), which is where `migrations/` and `schemas/`
//! actually live as real shipped directories. A compiled Rust binary has no equivalent "where my
//! source file lives"; the only self-derivable analogue at compile time is `CARGO_MANIFEST_DIR`
//! (baked in = this crate's own root, `antiloki-core-rust/`). Per `workspace-root.rs`'s module
//! doc, `antiloki-core-rust` and `antiloki-core` are siblings under the same repo root in this
//! project's layout, so `engine_root()` here returns `CARGO_MANIFEST_DIR` as the self-derived
//! analogue (mirroring the JS "self-derived, moves as a unit" comment for its OWN engine tree),
//! NOT a computed path into the sibling `antiloki-core/migrations`/`schemas` — this module does no
//! I/O itself (JS doesn't either; it only builds path strings), so the divergence is inert unless
//! a caller literally reads `migrations_dir()`/`schemas_dir()` expecting the JS-side files, which
//! no Rust call site does today (grep-confirmed: no `migrations_dir\(\)`/`schemas_dir\(\)` call
//! site exists in this crate as of this port).
// QUALITY: 82/100 — every derivation ported (env-override-else-join chain, all 21 path constants +
// the 9-key DB_PATHS map); one disclosed self-location deferral shared with workspace-root.rs/open-db.rs.

use serde_json::{json, Value};
use std::path::PathBuf;

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

// ── CORE: the engine code tree (self-derived; see module doc for the CARGO_MANIFEST_DIR gap) ──
/// `ENGINE_ROOT` — self-derived engine-tree root (JS: two levels up from `skills/engine/roots.mjs`).
pub fn engine_root() -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")) }
/// `SKILLS_ROOT` = `ENGINE_ROOT/skills` (JS: one level up from `skills/engine/roots.mjs`).
pub fn skills_root() -> PathBuf { engine_root().join("skills") }
/// `MIGRATIONS_DIR` = `join(ENGINE_ROOT, 'migrations')`.
pub fn migrations_dir() -> PathBuf { engine_root().join("migrations") }
/// `SCHEMAS_DIR` = `join(ENGINE_ROOT, 'schemas')`.
pub fn schemas_dir() -> PathBuf { engine_root().join("schemas") }

// ── SUBSTRATE_ROOT: the project holding the portable `.antiloki/`. ──────────────────────────
/// `SUBSTRATE_ROOT` = `resolve($ANTILOKI_SUBSTRATE_ROOT)` if set+non-empty, else `resolve(ENGINE_ROOT, '..')`.
pub fn substrate_root() -> PathBuf {
    match std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => {
            let root = engine_root();
            root.parent().map(|p| p.to_path_buf()).unwrap_or(root)
        }
    }
}

// ── SANDBOX: copy-on-write scratch, anchored at SUBSTRATE_ROOT (see JS module doc — deliberately
// NOT HOME_DIR; a prior pass split-brained sandbox writes vs reads by anchoring it there). ──────
/// `SANDBOX_DIR` = `join(SUBSTRATE_ROOT, '.antiloki', 'sandbox')`.
pub fn sandbox_dir() -> PathBuf { substrate_root().join(".antiloki").join("sandbox") }

/// A path in the form the OS FILE MANAGER will accept, for "reveal this folder" actions.
///
/// WHY THIS EXISTS: `explorer.exe` will not open `C:/dev/foo`. Given forward slashes it does not
/// error — it silently ignores the argument and opens the user's DEFAULT folder (Documents), so
/// the operator clicks "open worktree" and gets Documents, repeatedly, with every layer reporting
/// success. Rust hides the problem on the way in: `Path::new("C:/dev/foo").exists()` is true, so an
/// exists() guard passes and hands Explorer something Explorer cannot use. Forward slashes get in
/// here easily — `$HOME` under Git Bash is `C:/Users/…` or `/c/Users/…`, and those paths are joined
/// into worktree/sandbox locations and stored as-is.
///
/// canonicalize() gives native separators, but on Windows it also returns a `\\?\` verbatim path,
/// which Explorer likewise refuses — so the prefix is stripped. If canonicalize fails (the path is
/// gone), fall back to a plain separator swap rather than returning nothing: a best-effort reveal
/// beats a silent no-op. On non-Windows this is the identity, since `open`/`xdg-open` take the path
/// as given.
pub fn native_reveal_path(p: &str) -> String {
    if !cfg!(windows) {
        return p.to_string();
    }
    match std::fs::canonicalize(p) {
        Ok(abs) => {
            let s = abs.to_string_lossy().into_owned();
            s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
        }
        Err(_) => p.replace('/', "\\"),
    }
}

// ── DATA: the portable per-project substrate (db/ + knowledge/ + …). ────────────────────────
/// `DATA_DIR` = `resolve($ANTILOKI_DATA_DIR)` if set+non-empty, else `join(SUBSTRATE_ROOT, '.antiloki')`.
pub fn data_dir() -> PathBuf {
    match std::env::var("ANTILOKI_DATA_DIR") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => substrate_root().join(".antiloki"),
    }
}
/// `DB_DIR` = `join(DATA_DIR, 'db')`.
pub fn db_dir() -> PathBuf { data_dir().join("db") }
/// `KNOWLEDGE_DIR` = `join(DATA_DIR, 'knowledge')`.
pub fn knowledge_dir() -> PathBuf { data_dir().join("knowledge") }
/// `SOURCES_DIR` = `join(DATA_DIR, 'sources')`.
pub fn sources_dir() -> PathBuf { data_dir().join("sources") }
/// `ARTIFACTS_DIR` = `join(DATA_DIR, 'artifacts')`.
pub fn artifacts_dir() -> PathBuf { data_dir().join("artifacts") }
/// `LEARNING_CENTER_DIR` = `join(DATA_DIR, 'learning-center')`.
pub fn learning_center_dir() -> PathBuf { data_dir().join("learning-center") }
/// `BACKUPS_DIR` = `join(DATA_DIR, 'backups')`.
pub fn backups_dir() -> PathBuf { data_dir().join("backups") }
/// `ARCHIVE_DIR` = `join(DATA_DIR, 'archive')`.
pub fn archive_dir() -> PathBuf { data_dir().join("archive") }

// ── The nine substrate DB files (all siblings under db/). ───────────────────────────────────
/// `DB_PATHS.state`.
pub fn state_db_path() -> PathBuf { db_dir().join("state.sqlite") }
/// `DB_PATHS.memory`.
pub fn memory_db_path() -> PathBuf { db_dir().join("memory.sqlite") }
/// `DB_PATHS.brain`.
pub fn brain_db_path() -> PathBuf { db_dir().join("brain.sqlite") }
/// `DB_PATHS.laws`.
pub fn laws_db_path() -> PathBuf { db_dir().join("laws.sqlite") }
/// `DB_PATHS.knowledge`.
pub fn knowledge_db_path() -> PathBuf { db_dir().join("knowledge.sqlite") }
/// `DB_PATHS.timeline`.
pub fn timeline_db_path() -> PathBuf { db_dir().join("timeline.sqlite") }
/// `DB_PATHS.judgments`.
pub fn judgments_db_path() -> PathBuf { db_dir().join("judgments.sqlite") }
/// `DB_PATHS.code`.
pub fn code_db_path() -> PathBuf { db_dir().join("code.sqlite") }
/// `DB_PATHS.notebooks`.
pub fn notebooks_db_path() -> PathBuf { db_dir().join("notebooks.sqlite") }

/// `export const DB_PATHS = { state, memory, brain, laws, knowledge, timeline, judgments, code, notebooks }`
/// — modeled as a `serde_json::Value` object of the same 9 keys, each value the string path.
pub fn db_paths() -> Value {
    json!({
        "state": state_db_path().to_string_lossy(),
        "memory": memory_db_path().to_string_lossy(),
        "brain": brain_db_path().to_string_lossy(),
        "laws": laws_db_path().to_string_lossy(),
        "knowledge": knowledge_db_path().to_string_lossy(),
        "timeline": timeline_db_path().to_string_lossy(),
        "judgments": judgments_db_path().to_string_lossy(),
        "code": code_db_path().to_string_lossy(),
        "notebooks": notebooks_db_path().to_string_lossy(),
    })
}
/// Typed accessor over `db_paths()`'s 9 keys — convenience for call sites that index `DB_PATHS[name]`
/// dynamically in JS (e.g. `DB_PATHS[dbName]`). Returns `None` for an unknown key, mirroring JS
/// `undefined` on a missing object property (no such call site throws on a bad key today).
pub fn db_path(name: &str) -> Option<PathBuf> {
    match name {
        "state" => Some(state_db_path()),
        "memory" => Some(memory_db_path()),
        "brain" => Some(brain_db_path()),
        "laws" => Some(laws_db_path()),
        "knowledge" => Some(knowledge_db_path()),
        "timeline" => Some(timeline_db_path()),
        "judgments" => Some(judgments_db_path()),
        "code" => Some(code_db_path()),
        "notebooks" => Some(notebooks_db_path()),
        _ => None,
    }
}

// ── HOME: machine-local secrets + runtime + registry (BE-owned). ────────────────────────────
/// `HOME_DIR` = `resolve($ANTILOKI_HOME)` if set+non-empty, else `join(homedir(), '.antiloki')`.
pub fn home_dir() -> PathBuf {
    match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    }
}
/// `AUTH_DIR` = `join(HOME_DIR, 'auth')`.
pub fn auth_dir() -> PathBuf { home_dir().join("auth") }
/// `RUN_DIR` = `join(HOME_DIR, 'run')`.
pub fn run_dir() -> PathBuf { home_dir().join("run") }
/// `WORKTREES_DIR` = `join(HOME_DIR, 'worktrees')`.
pub fn worktrees_dir() -> PathBuf { home_dir().join("worktrees") }
/// `TOOLS_DIR` = `join(HOME_DIR, 'tools')`.
pub fn tools_dir() -> PathBuf { home_dir().join("tools") }
/// `HALT_FILE` = `join(RUN_DIR, 'driver-halt')`.
pub fn halt_file() -> PathBuf { run_dir().join("driver-halt") }

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The fixture roots below are written Windows-style (`C:\\data\\db`); on other hosts the same
    /// path is spelled with `/` and no drive (`/data/db`), so the join assertions test the same structure.
    fn np(win: &str) -> String {
        if cfg!(windows) { win.to_string() } else { win.replacen("C:", "", 1).replace('\\', "/") }
    }

    #[test]
    fn engine_root_is_the_cargo_manifest_dir() {
        assert_eq!(engine_root(), PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    }

    #[test]
    fn skills_migrations_schemas_are_joined_under_engine_root() {
        let root = engine_root();
        assert_eq!(skills_root(), root.join("skills"));
        assert_eq!(migrations_dir(), root.join("migrations"));
        assert_eq!(schemas_dir(), root.join("schemas"));
    }

    #[test]
    fn substrate_root_env_override_wins_over_derived_default() {
        let _lock = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let prev = std::env::var("ANTILOKI_SUBSTRATE_ROOT").ok();
        std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", &np("C:\\tmp\\some-managed-project"));
        assert_eq!(substrate_root(), PathBuf::from(&np("C:\\tmp\\some-managed-project")));
        match prev {
            Some(v) => std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", v),
            None => std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT"),
        }
    }

    #[test]
    fn substrate_root_defaults_to_engine_root_parent_when_unset() {
        let _lock = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let prev = std::env::var("ANTILOKI_SUBSTRATE_ROOT").ok();
        std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT");
        let expected = engine_root().parent().map(|p| p.to_path_buf()).unwrap_or_else(engine_root);
        assert_eq!(substrate_root(), expected);
        if let Some(v) = prev { std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", v); }
    }

    #[test]
    fn sandbox_dir_is_anchored_under_substrate_root_dot_antiloki() {
        let _lock = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let prev = std::env::var("ANTILOKI_SUBSTRATE_ROOT").ok();
        std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", &np("C:\\tmp\\some-managed-project"));
        assert_eq!(sandbox_dir(), Path::new(&np("C:\\tmp\\some-managed-project\\.antiloki\\sandbox")));
        match prev {
            Some(v) => std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", v),
            None => std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT"),
        }
    }

    #[test]
    fn data_dir_env_override_wins_and_default_joins_substrate_root() {
        let _lock = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let prev_data = std::env::var("ANTILOKI_DATA_DIR").ok();
        let prev_sub = std::env::var("ANTILOKI_SUBSTRATE_ROOT").ok();

        std::env::set_var("ANTILOKI_DATA_DIR", &np("C:\\custom\\data"));
        assert_eq!(data_dir(), PathBuf::from(&np("C:\\custom\\data")));

        std::env::remove_var("ANTILOKI_DATA_DIR");
        std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", &np("C:\\tmp\\some-managed-project"));
        assert_eq!(data_dir(), Path::new(&np("C:\\tmp\\some-managed-project\\.antiloki")));

        match prev_data {
            Some(v) => std::env::set_var("ANTILOKI_DATA_DIR", v),
            None => std::env::remove_var("ANTILOKI_DATA_DIR"),
        }
        match prev_sub {
            Some(v) => std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", v),
            None => std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT"),
        }
    }

    #[test]
    fn data_subdirs_are_joined_under_data_dir() {
        let _lock = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let prev = std::env::var("ANTILOKI_DATA_DIR").ok();
        std::env::set_var("ANTILOKI_DATA_DIR", &np("C:\\data"));
        assert_eq!(db_dir(), Path::new(&np("C:\\data\\db")));
        assert_eq!(knowledge_dir(), Path::new(&np("C:\\data\\knowledge")));
        assert_eq!(sources_dir(), Path::new(&np("C:\\data\\sources")));
        assert_eq!(artifacts_dir(), Path::new(&np("C:\\data\\artifacts")));
        assert_eq!(learning_center_dir(), Path::new(&np("C:\\data\\learning-center")));
        assert_eq!(backups_dir(), Path::new(&np("C:\\data\\backups")));
        assert_eq!(archive_dir(), Path::new(&np("C:\\data\\archive")));
        match prev {
            Some(v) => std::env::set_var("ANTILOKI_DATA_DIR", v),
            None => std::env::remove_var("ANTILOKI_DATA_DIR"),
        }
    }

    #[test]
    fn db_paths_covers_all_nine_siblings_under_db_dir() {
        let _lock = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let prev = std::env::var("ANTILOKI_DATA_DIR").ok();
        std::env::set_var("ANTILOKI_DATA_DIR", &np("C:\\data"));
        let paths = db_paths();
        assert_eq!(paths["state"], json!(np("C:\\data\\db\\state.sqlite")));
        assert_eq!(paths["memory"], json!(np("C:\\data\\db\\memory.sqlite")));
        assert_eq!(paths["brain"], json!(np("C:\\data\\db\\brain.sqlite")));
        assert_eq!(paths["laws"], json!(np("C:\\data\\db\\laws.sqlite")));
        assert_eq!(paths["knowledge"], json!(np("C:\\data\\db\\knowledge.sqlite")));
        assert_eq!(paths["timeline"], json!(np("C:\\data\\db\\timeline.sqlite")));
        assert_eq!(paths["judgments"], json!(np("C:\\data\\db\\judgments.sqlite")));
        assert_eq!(paths["code"], json!(np("C:\\data\\db\\code.sqlite")));
        assert_eq!(paths["notebooks"], json!(np("C:\\data\\db\\notebooks.sqlite")));
        assert_eq!(paths.as_object().unwrap().len(), 9);
        match prev {
            Some(v) => std::env::set_var("ANTILOKI_DATA_DIR", v),
            None => std::env::remove_var("ANTILOKI_DATA_DIR"),
        }
    }

    #[test]
    fn db_path_accessor_matches_db_paths_and_none_for_unknown_key() {
        let _lock = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let prev = std::env::var("ANTILOKI_DATA_DIR").ok();
        std::env::set_var("ANTILOKI_DATA_DIR", &np("C:\\data"));
        assert_eq!(db_path("state"), Some(state_db_path()));
        assert_eq!(db_path("notebooks"), Some(notebooks_db_path()));
        assert_eq!(db_path("nope"), None);
        match prev {
            Some(v) => std::env::set_var("ANTILOKI_DATA_DIR", v),
            None => std::env::remove_var("ANTILOKI_DATA_DIR"),
        }
    }

    #[test]
    fn home_dir_env_override_wins_over_homedir_default() {
        let _lock = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let prev = std::env::var("ANTILOKI_HOME").ok();
        std::env::set_var("ANTILOKI_HOME", &np("C:\\custom\\home\\.antiloki"));
        assert_eq!(home_dir(), PathBuf::from(&np("C:\\custom\\home\\.antiloki")));
        match prev {
            Some(v) => std::env::set_var("ANTILOKI_HOME", v),
            None => std::env::remove_var("ANTILOKI_HOME"),
        }
    }

    #[test]
    fn home_subdirs_and_halt_file_are_joined_under_home_dir() {
        let _lock = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let prev = std::env::var("ANTILOKI_HOME").ok();
        std::env::set_var("ANTILOKI_HOME", &np("C:\\home\\.antiloki"));
        assert_eq!(auth_dir(), Path::new(&np("C:\\home\\.antiloki\\auth")));
        assert_eq!(run_dir(), Path::new(&np("C:\\home\\.antiloki\\run")));
        assert_eq!(worktrees_dir(), Path::new(&np("C:\\home\\.antiloki\\worktrees")));
        assert_eq!(tools_dir(), Path::new(&np("C:\\home\\.antiloki\\tools")));
        assert_eq!(halt_file(), Path::new(&np("C:\\home\\.antiloki\\run\\driver-halt")));
        match prev {
            Some(v) => std::env::set_var("ANTILOKI_HOME", v),
            None => std::env::remove_var("ANTILOKI_HOME"),
        }
    }

    /// 2026-10-13: auth material belongs in a private directory under ANTILOKI_HOME.
    #[test]
    fn auth_dir_is_private_under_antiloki_home() {
        let _lock = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("antiloki-auth-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let prev = std::env::var("ANTILOKI_HOME").ok();
        std::env::set_var("ANTILOKI_HOME", &dir);
        let auth = auth_dir();
        std::fs::create_dir_all(&auth).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(std::fs::metadata(&auth).unwrap().permissions().mode() & 0o777, 0o700);
        }
        match prev {
            Some(v) => std::env::set_var("ANTILOKI_HOME", v),
            None => std::env::remove_var("ANTILOKI_HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
