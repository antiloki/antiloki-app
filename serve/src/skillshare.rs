//! THE SKILLS PANE'S BACKEND (operator 2026-11-10: "the skillshare pane still asking to install skillshare: no, we will have our own ... not using others' project"). The work is `skillbank.rs`: antiloki's own bank of skills and agents, linked into every AI tool on the machine.
//! This only carries the pane's two calls to it:
//!
//!   GET  /api/v1/skillshare/state?project=1&checks=1   the bank and what a sync would do, per scope (global, and the open project's when it has a bank)
//!   POST /api/v1/skillshare/run                        `{op: "sync"|"check"|"update"|"uninstall"|"install"|"new"|"toggle", scope, name?, kind?, source?, description?, all?, off?}`
//! The route names are kept so the screen did not change its calls.
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use crate::dispatch::AppState;

fn reply(s: StatusCode, v: Value) -> Response { (s, Json(v)).into_response() }
fn home() -> PathBuf { std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default() }
fn project_root(state: &Arc<AppState>, wanted: bool) -> Option<PathBuf> { if wanted { Some(crate::chat_pane::workspace_root_of(state)).filter(|r| !r.is_empty()).map(PathBuf::from) } else { None } }

pub async fn state_get(state: Arc<AppState>, q: std::collections::HashMap<String, String>) -> Response {
    let root = project_root(&state, q.get("project").map_or(false, |v| v == "1")); let checks = q.get("checks").map_or(false, |v| v == "1");
    let v = tokio::task::spawn_blocking(move || crate::skillbank::read_state(&home(), root.as_deref(), checks)).await.unwrap_or_else(|_| json!({ "ok": false, "error": "the read did not finish" }));
    reply(StatusCode::OK, v)
}
pub async fn run_post(state: Arc<AppState>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null); let root = project_root(&state, b["scope"] == "project");
    let v = tokio::task::spawn_blocking(move || crate::skillbank::perform(&home(), root.as_deref(), &b)).await.unwrap_or_else(|_| json!({ "ok": false, "error": "the run did not finish" }));
    reply(StatusCode::OK, v)
}
