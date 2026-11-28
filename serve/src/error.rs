//! The error contract, ported verbatim from the BE.
//!
//! Two files' worth of behaviour live here, and both had to be copied EXACTLY rather than
//! redesigned, because the FE branches on what they produce:
//!
//!   * `engine.service.ts toAntilokiError()` — the engine returns `Err(String)`, so the code is
//!     recovered by matching the message text. This is not a hack the cutover gets to remove: the
//!     crate still does not thread codes through its ~1150 `Err(String)` returns, so deleting the
//!     recovery would turn every typed failure back into a 500. Same patterns, same order.
//!   * `engine-error.filter.ts CODE_TO_STATUS` — code → HTTP status.
//!
//! W0 measured what this produces: 25 of 32 observed failures used the engine envelope. The
//! remaining 7 came from Nest's own pipes and are NOT this module's job (see dispatch.rs).

use axum::http::StatusCode;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;

pub struct EngineError {
    pub code: &'static str,
    pub cause: String,
    pub action: String,
}

fn re(pattern: &'static str, cell: &'static OnceLock<Regex>) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("static pattern compiles"))
}

/// Port of `toAntilokiError(e, action)`. Pattern order is load-bearing — SKILL_NOT_FOUND is
/// checked before the broad "not found" contains(), exactly as in the original if/else chain.
pub fn to_antiloki_error(message: &str, action: &str) -> EngineError {
    static STRIP: OnceLock<Regex> = OnceLock::new();
    static ACTION_MISSING: OnceLock<Regex> = OnceLock::new();
    static LAW_DENIED: OnceLock<Regex> = OnceLock::new();
    static AUTH_REJECTED: OnceLock<Regex> = OnceLock::new();
    static NOT_PERMITTED: OnceLock<Regex> = OnceLock::new();
    static BAD_STEPS: OnceLock<Regex> = OnceLock::new();

    let cause = re(r"^action-run:\s*", &STRIP).replace(message, "").to_string();
    let lower = cause.to_lowercase();

    let code = if re(r"action '.*' not found", &ACTION_MISSING).is_match(&cause) {
        "SKILL_NOT_FOUND"
    } else if lower.contains("not found") || lower.contains("is not registered") {
        "NOT_FOUND"
    } else if lower.contains(" is required") {
        "VALIDATION_REQUIRED"
    } else if re(r"^law '.*' denied:", &LAW_DENIED).is_match(&cause) {
        "PERMISSION_DENIED"
    } else if re(r"^auth token rejected:", &AUTH_REJECTED).is_match(&cause) {
        "PERMISSION_DENIED"
    } else if re(
        r"(?i)\b(?:not allowed|not permitted|permission denied)\b",
        &NOT_PERMITTED,
    )
    .is_match(&cause)
    {
        "PERMISSION_DENIED"
    } else if re(r"^invalid steps_json for ", &BAD_STEPS).is_match(&cause) {
        "PARSE_ERROR"
    // A CLIENT'S MISTAKE IS NOT A SERVER FAULT (QA report BUG-001, 2026-10-06, confirmed live): `workspaces/add` answered a duplicate name, a missing folder and a bad slug with
    // HTTP 500 INTERNAL_ERROR, and `workspaces/activate` did the same for a workspace that is not registered. Their messages matched none of the shapes above. Added AFTER the
    // permission and parse checks so nothing already classified moves.
    } else if lower.contains("is already registered") || lower.contains("already exists") {
        "ALREADY_EXISTS"
    } else if lower.contains("does not exist on disk") || lower.contains(" must be ") {
        "VALIDATION_LOGIC"
    } else {
        "INTERNAL_ERROR"
    };

    EngineError { code, cause, action: action.to_string() }
}

/// Port of `CODE_TO_STATUS`. Unknown codes fall to 500, same as the original `?? 500`.
pub fn code_to_status(code: &str) -> StatusCode {
    match code {
        "VALIDATION_REQUIRED" | "VALIDATION_TYPE" | "VALIDATION_ENUM" | "VALIDATION_LOGIC"
        | "CONTEXT_MISSING" => StatusCode::BAD_REQUEST,
        "NOT_FOUND" | "SKILL_NOT_FOUND" => StatusCode::NOT_FOUND,
        "ALREADY_EXISTS" => StatusCode::CONFLICT,
        "PERMISSION_DENIED" => StatusCode::FORBIDDEN,
        "IO_ERROR" | "PARSE_ERROR" => StatusCode::BAD_GATEWAY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

impl EngineError {
    /// The exact body EngineErrorFilter emits for a typed engine error. `skill` is always
    /// "action-run" because that is what toAntilokiError stamps, and `context` is `{action}` —
    /// deliberately NOT `{action, engine}`, so the shape is identical across engines.
    pub fn body(&self) -> Value {
        json!({
            "error": {
                "code": self.code,
                "skill": "action-run",
                "cause": self.cause,
                "context": { "action": self.action },
            }
        })
    }
}

/// The filter's final branch: an exception that is not an AntilokiError. Note the shape is
/// NARROWER than the typed one — no `skill`, no `context`. Reproduced as-is.
pub fn unhandled_body(cause: &str) -> Value {
    json!({ "error": { "code": "INTERNAL_ERROR", "cause": cause } })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_of(msg: &str) -> &'static str {
        to_antiloki_error(msg, "some.action").code
    }

    #[test]
    fn recovers_the_action_run_vocabulary() {
        // Pattern order is load-bearing: SKILL_NOT_FOUND is checked before the broad
        // "not found" contains(), so a missing ACTION does not degrade to NOT_FOUND.
        assert_eq!(code_of("action 'x.y' not found"), "SKILL_NOT_FOUND");
        assert_eq!(code_of("codebase-file-read: file not found"), "NOT_FOUND");
        assert_eq!(code_of("card-add: slug is required"), "VALIDATION_REQUIRED");
        assert_eq!(code_of("law 'X' denied: nope"), "PERMISSION_DENIED");
        assert_eq!(code_of("auth token rejected: bad"), "PERMISSION_DENIED");
        assert_eq!(code_of("that is not allowed here"), "PERMISSION_DENIED");
        assert_eq!(code_of("invalid steps_json for foo"), "PARSE_ERROR");
        assert_eq!(code_of("something else entirely"), "INTERNAL_ERROR");
    }

    #[test]
    fn a_clients_mistake_is_a_4xx_not_a_server_fault() {
        // the four messages the QA report's BUG-001 got as HTTP 500, verbatim from `workspace-add` / `workspace-activate`
        let add_dup = to_antiloki_error("workspace-add: workspace 'ws-main' is already registered", "system.workspace.add");
        assert_eq!((add_dup.code, code_to_status(add_dup.code)), ("ALREADY_EXISTS", StatusCode::CONFLICT));
        let add_gone = to_antiloki_error("workspace-add: root_path does not exist on disk: '/tmp/nope'", "system.workspace.add");
        assert_eq!((add_gone.code, code_to_status(add_gone.code)), ("VALIDATION_LOGIC", StatusCode::BAD_REQUEST));
        let add_slug = to_antiloki_error("workspace-add: workspace name must be a simple slug ([a-z0-9._-], no spaces): got 'Bad Name!'", "system.workspace.add");
        assert_eq!((add_slug.code, code_to_status(add_slug.code)), ("VALIDATION_LOGIC", StatusCode::BAD_REQUEST));
        let act = to_antiloki_error("workspace-activate: workspace 'x' is not registered", "system.workspace.activate");
        assert_eq!((act.code, code_to_status(act.code)), ("NOT_FOUND", StatusCode::NOT_FOUND));
        // the cause text is untouched, so a client still shows the reason
        assert!(add_dup.cause.contains("already registered"));
        // nothing already classified moves: a denied law that happens to say "must be" stays a permission error, a missing action stays SKILL_NOT_FOUND
        assert_eq!(code_of("law 'X' denied: path must be inside the worktree"), "PERMISSION_DENIED");
        assert_eq!(code_of("action 'a.b' not found"), "SKILL_NOT_FOUND");
        assert_eq!(code_of("cost-settings: op must be one of: get, set (got 'x')"), "VALIDATION_LOGIC");
        // and a real fault is still a fault
        assert_eq!(code_of("database disk image is malformed"), "INTERNAL_ERROR");
    }

    #[test]
    fn strips_the_action_run_prefix_from_cause() {
        let e = to_antiloki_error("action-run: card-add: slug is required", "board-keeper.card.add");
        assert_eq!(e.cause, "card-add: slug is required");
        assert_eq!(e.code, "VALIDATION_REQUIRED");
    }

    #[test]
    fn status_mapping_matches_the_nest_filter() {
        assert_eq!(code_to_status("VALIDATION_REQUIRED"), StatusCode::BAD_REQUEST);
        assert_eq!(code_to_status("CONTEXT_MISSING"), StatusCode::BAD_REQUEST);
        assert_eq!(code_to_status("NOT_FOUND"), StatusCode::NOT_FOUND);
        assert_eq!(code_to_status("SKILL_NOT_FOUND"), StatusCode::NOT_FOUND);
        assert_eq!(code_to_status("PERMISSION_DENIED"), StatusCode::FORBIDDEN);
        assert_eq!(code_to_status("IO_ERROR"), StatusCode::BAD_GATEWAY);
        assert_eq!(code_to_status("PARSE_ERROR"), StatusCode::BAD_GATEWAY);
        assert_eq!(code_to_status("anything-unknown"), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn typed_envelope_carries_skill_and_context() {
        let e = to_antiloki_error("card-add: slug is required", "board-keeper.card.add");
        assert_eq!(
            e.body().to_string(),
            r#"{"error":{"code":"VALIDATION_REQUIRED","skill":"action-run","cause":"card-add: slug is required","context":{"action":"board-keeper.card.add"}}}"#
        );
    }

    #[test]
    fn unhandled_envelope_is_narrower_than_the_typed_one() {
        // The filter's final branch emits no `skill` and no `context`. Reproduced as-is.
        assert_eq!(
            unhandled_body("boom").to_string(),
            r#"{"error":{"code":"INTERNAL_ERROR","cause":"boom"}}"#
        );
    }
}
