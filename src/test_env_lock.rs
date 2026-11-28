//! test_env_lock — sentinel-side testing utility (like js_math/db_json, not a core twin).
//!
//! `ANTILOKI_HOME` (and similar env vars) is process-global, but `cargo test` runs the whole
//! crate's tests in parallel threads by default. Multiple twins (god-driver-halt, god-driver-
//! resume, tui-god-token, lint-detect, typecheck-detect, sandbox-resolve-path,
//! knowledge-document-add, ollama-host, orphan-sandbox-dirs, orphan-worktree-dirs, byok-key-set,
//! driver-halt, ...) each need to override this env var in their own `#[cfg(test)]` module. Each
//! file previously defined its OWN local `static ENV_LOCK: Mutex<()>` — which does NOT prevent a
//! race between DIFFERENT files' tests, since each local mutex only serializes calls within its
//! own module. Two files' tests could both acquire their own (uncontended) lock and mutate the
//! same real env var simultaneously, producing cross-test corruption (verified: lint-detect's
//! test observed a path string that belonged to typecheck-detect's concurrently-running test).
//!
//! Every twin that needs to override a process-global env var under test must lock THIS shared
//! mutex, not define its own.
//! POISON TOLERANCE: this guards a process-global env var, not a data invariant — a test that
//! panics while holding it leaves nothing corrupted, since the next test sets the var it needs
//! anyway. A bare `Mutex` would poison on that panic and every one of the 157 call sites would
//! then fail with `PoisonError` instead of its own real result. (Measured: one panicking test
//! turned into 462 of 567 suite failures, burying the ~105 genuine ones.) So `lock()` recovers the
//! guard from a poisoned lock, and returns `Result` purely so the existing
//! `ENV_LOCK.lock().unwrap()` call sites keep compiling unchanged.
#[cfg(test)]
pub struct EnvLock(std::sync::Mutex<()>);

#[cfg(test)]
impl EnvLock {
    pub fn lock(&self) -> Result<std::sync::MutexGuard<'_, ()>, std::convert::Infallible> {
        Ok(self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
    }
}

#[cfg(test)]
pub static ENV_LOCK: EnvLock = EnvLock(std::sync::Mutex::new(()));
