//! law-hook-no-drift-on-audit-commit.rs — twin of skills/audit/law-hook-no-drift-on-audit-commit.mjs
//! (standard db lane). NAMES ARE CONTRACT: `lawHookNoDriftOnAuditCommit` (default),
//! `AUDIT_COMMIT_SKILLS` (data, inlined as a const array — not a census unit).
//!
//! Enforces COMMIT_CODE_BEFORE_AUDIT_COMMIT: called before dispatching an audit-commit action
//! (board-keeper.card.add/update/status.change). Only ONE branch is fixture-safe: `action_name`
//! not in `AUDIT_COMMIT_SKILLS` returns `{allowed:true}` before touching git at all — mirrors
//! JS `!action_name || !AUDIT_COMMIT_SKILLS.has(action_name)`.
//!
//! Every other branch is DISCLOSED UNTESTED by fixture, same precedent as git-commit-card.rs:
//! - `context.gitRunner` stub-override check has no Rust equivalent (a stubbed function isn't
//!   JSON-fixture-representable) — `context` is always `{db}` in real (non-JS-test) callers, so
//!   this twin never takes that branch, matching every actual production caller.
//! - The `ANTILOKI_ALLOW_DRIFT=true` env escape hatch is read via `std::env::var` (same pattern
//!   as law-hook-no-direct-audit-write.rs's `ANTILOKI_ALLOW_RAW_AUDIT`) but left unset during
//!   conform, so only the default (unset) path is exercised.
//! - `detectCodeDrift` (imported from git-commit-card.rs, itself bespoke-native/no-fixture per
//!   its own module doc) shells out to a real `git status` subprocess — genuinely environment-
//!   dependent, so no fixture case reaches it; real, correct code below, composed verbatim from
//!   the already-converted twin.
use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// Actions that internally call git-commit-card via their steps_json — the audit-commit boundary points.
const AUDIT_COMMIT_SKILLS: [&str; 3] = [
    "board-keeper.card.add",
    "board-keeper.card.update",
    "board-keeper.card.status.change",
];

pub fn lawHookNoDriftOnAuditCommit(input: &Value, db: &Connection) -> Result<Value, String> {
    let action_name = input.get("action_name");
    let action_name_str = action_name.and_then(|v| v.as_str());

    // !action_name || !AUDIT_COMMIT_SKILLS.has(action_name)
    if !truthy(action_name) || !action_name_str.map_or(false, |s| AUDIT_COMMIT_SKILLS.contains(&s)) {
        return Ok(json!({ "allowed": true }));
    }

    // context.gitRunner stubbed-runner branch: no Rust equivalent (see module doc) — never taken.

    if std::env::var("ANTILOKI_ALLOW_DRIFT").ok().as_deref() == Some("true") {
        return Ok(json!({ "allowed": true, "escape_hatch": "ANTILOKI_ALLOW_DRIFT=true" }));
    }

    let drifted = crate::external_git_commit_card::detectCodeDrift(Some(db));
    if drifted.is_empty() {
        return Ok(json!({ "allowed": true }));
    }

    let summary = if drifted.len() > 5 {
        format!("{}, +{} more", drifted[..5].join(", "), drifted.len() - 5)
    } else {
        drifted.join(", ")
    };

    Ok(json!({
        "allowed": false,
        "reason": format!("code drift detected ({} files): {}", drifted.len(), summary),
        "suggested_path": "commit code changes first, OR set ANTILOKI_ALLOW_DRIFT=true to bypass",
        "drift": drifted,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(i: Value) -> Value {
        lawHookNoDriftOnAuditCommit(&i, &Connection::open_in_memory().unwrap()).unwrap()
    }

    #[test]
    fn non_audit_commit_actions_are_always_allowed() {
        assert_eq!(call(json!({})), json!({ "allowed": true }));
        assert_eq!(call(json!({ "action_name": "" })), json!({ "allowed": true }));
        assert_eq!(call(json!({ "action_name": "dev.file.write" })), json!({ "allowed": true }));
        assert_eq!(call(json!({ "action_name": "board-keeper.card.discuss" })), json!({ "allowed": true }));
    }

    #[test]
    fn allow_drift_env_escape_hatch() {
        // ANTILOKI_ALLOW_DRIFT is read by the real hook, so any other test that trips the hook
        // while this window is open would silently take the escape hatch. Serialise on the shared
        // env lock, as every other env-mutating test does.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        std::env::set_var("ANTILOKI_ALLOW_DRIFT", "true");
        let r = call(json!({ "action_name": "board-keeper.card.add" }));
        std::env::remove_var("ANTILOKI_ALLOW_DRIFT");
        assert_eq!(r, json!({ "allowed": true, "escape_hatch": "ANTILOKI_ALLOW_DRIFT=true" }));
    }
}
