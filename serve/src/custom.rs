//! Hand-written handlers for the tail of the surface.
//!
//! The generated table covers 325 routes because they share ONE shape: bind input, dispatch, map
//! the error. What remains is a set of one-offs — a static registry, fire-and-forget telemetry, a
//! detached dispatch, a dynamic action chosen by the body. Extending the template language to
//! cover them would turn it into a general-purpose interpreter; each is clearer written out.
//!
//! Everything here still goes through the same dispatch helpers, so attribution, the error
//! envelopes, nil-means-empty-body and number normalization stay identical to the generated path.

use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::dispatch::{engine_call, AppState};

/// `POST /api/v1/upload` — multipart ingest of a knowledge document.
///
/// The BE uses Nest's FileInterceptor on field "file"; here it is axum's Multipart. The file is
/// base64'd into the dispatch input, so the engine gets bytes without a shared filesystem.
pub async fn upload(state: Arc<AppState>, headers: HeaderMap, mut mp: axum::extract::Multipart) -> Response {
    let mut filename = String::new();
    let mut mime = String::new();
    let mut bytes: Option<Vec<u8>> = None;
    while let Ok(Some(field)) = mp.next_field().await {
        let is_file_part = field.file_name().is_some();
        match field.name() {
            Some("file") => {
                filename = field.file_name().unwrap_or_default().to_string();
                mime = field.content_type().unwrap_or_default().to_string();
                bytes = field.bytes().await.ok().map(|b| b.to_vec());
                break;
            }
            // multer's `.single('file')` rejects any OTHER file part with LIMIT_UNEXPECTED_FILE
            // BEFORE the handler runs, so the handler's own "no file provided" message is
            // unreachable in that case. Plain text fields are allowed through.
            _ if is_file_part => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "message": "Unexpected field", "error": "Bad Request", "statusCode": 400 })),
                )
                    .into_response()
            }
            _ => {}
        }
    }
    let Some(bytes) = bytes else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "message": "no file provided (expected multipart field \"file\")",
                "error": "Bad Request",
                "statusCode": 400
            })),
        )
            .into_response();
    };

    let input = json!({
        "filename": filename,
        "mime": mime,
        "bytes_base64": base64_std(&bytes),
    });
    let ctx = crate::dispatch::attribution_ctx(&headers);
    crate::dispatch::finish(
        engine_call(&state, "system.knowledge.document.ingest", input, ctx).await,
        "POST",
        "system.knowledge.document.ingest",
    )
}

/// `POST /api/v1/upload/path {path}` — the same ingest for a file the OS dialog picked (the desktop shell has the
/// path, not the bytes; the serve runs on the same machine and reads it). Bounded at 25 MB; a directory or an
/// unreadable path is a 400 with the reason.
pub async fn upload_path(state: Arc<AppState>, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    let b: serde_json::Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let path = b.get("path").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let bad = |m: String| (StatusCode::BAD_REQUEST, Json(json!({ "message": m, "error": "Bad Request", "statusCode": 400 }))).into_response();
    if path.is_empty() { return bad("path is required".into()); }
    let p = std::path::Path::new(&path);
    if !p.is_file() { return bad(format!("{path} is not a file")); }
    let meta = match std::fs::metadata(p) { Ok(m) => m, Err(e) => return bad(format!("{path}: {e}")) };
    if meta.len() > 25 * 1024 * 1024 { return bad(format!("{path} is {} MB — the limit is 25 MB", meta.len() / 1_048_576)); }
    let bytes = match std::fs::read(p) { Ok(b) => b, Err(e) => return bad(format!("{path}: {e}")) };
    let filename = p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let mime = match p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("md") | Some("markdown") => "text/markdown", Some("txt") | Some("log") => "text/plain", Some("json") => "application/json", Some("pdf") => "application/pdf",
        Some("png") => "image/png", Some("jpg") | Some("jpeg") => "image/jpeg", Some("csv") => "text/csv", Some("html") | Some("htm") => "text/html", _ => "application/octet-stream",
    };
    let input = json!({ "filename": filename, "mime": mime, "bytes_base64": base64_std(&bytes), "origin_path": path });
    let ctx = crate::dispatch::attribution_ctx(&headers);
    crate::dispatch::finish(engine_call(&state, "system.knowledge.document.ingest", input, ctx).await, "POST", "system.knowledge.document.ingest")
}

fn base64_std(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// `POST /api/v1/codebase/onboard/start` — start (or resume) an atlas onboard job.
///
/// The BE offloads the long job to a detached NODE child (`scripts/onboard-runner.mjs`) and falls
/// back to an in-process detached dispatch when that runner is missing, logging
/// "onboard-runner not found — falling back to in-process". There is no Node runner in this
/// world — and there will be none in a shipped binary — so the fallback IS the path here. It is
/// the BE's own documented behaviour, not an improvisation.
pub async fn codebase_onboard_start(state: Arc<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
    let ctx = crate::dispatch::attribution_ctx(&headers);

    let resume_job_id = b.get("resume_job_id").filter(|v| match v {
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::Bool(x) => *x,
        _ => true,
    });

    if let Some(job_id) = resume_job_id.cloned() {
        let marked = match engine_call(
            &state,
            "system.codebase.onboard.control",
            json!({ "job_id": job_id, "op": "resume-mark" }),
            ctx.clone(),
        )
        .await
        {
            Ok((v, _)) => v,
            Err(e) => return crate::dispatch::finish(Err(e), "POST", "system.codebase.onboard.control"),
        };

        let st = state.clone();
        let jid = job_id.clone();
        tokio::spawn(async move {
            // Detached; a failure is recorded on the job row, so it is swallowed here.
            let _ = engine_call(
                &st,
                "system.codebase.onboard",
                json!({ "job_id": jid, "resume": true }),
                json!({ "actor": "web-fe", "source": "web-fe" }),
            )
            .await;
        });

        return (
            StatusCode::CREATED,
            Json(json!({
                "job_id": job_id,
                "root": marked.get("root").cloned().unwrap_or(Value::Null),
                "phase": marked.get("phase").cloned().unwrap_or(json!("ingest")),
                "status": "running",
                "resumed": true,
            })),
        )
            .into_response();
    }

    // the rebuild itself runs on the SAME workspace the request names (a team's tree is one): it used to run on the open workspace and leave the other's job queued for ever
    let ctx_bg = { let mut c = json!({ "actor": "web-fe", "source": "web-fe" }); if let Some(r) = ctx.get("workspace_root") { c["workspace_root"] = r.clone(); } c };
    let started = match engine_call(
        &state,
        "system.codebase.onboard.start",
        json!({ "root": b.get("root").cloned().unwrap_or(Value::Null) }),
        ctx,
    )
    .await
    {
        Ok((v, _)) => v,
        Err(e) => return crate::dispatch::finish(Err(e), "POST", "system.codebase.onboard.start"),
    };

    // `started?.job_id ?? started?.result?.job_id` — the action has been seen wrapping its payload.
    let pick = |k: &str| -> Option<Value> {
        started
            .get(k)
            .cloned()
            .or_else(|| started.get("result").and_then(|r| r.get(k)).cloned())
            .filter(|v| !v.is_null())
    };
    let job_id = pick("job_id");
    let already_running = pick("already_running").and_then(|v| v.as_bool()).unwrap_or(false);

    if let (Some(jid), false) = (job_id, already_running) {
        let st = state.clone();
        let mut input = b.clone();
        if let Some(o) = input.as_object_mut() {
            o.insert("job_id".into(), jid);
        }
        tokio::spawn(async move {
            let _ = engine_call(
                &st,
                "system.codebase.onboard",
                input,
                ctx_bg,
            )
            .await;
        });
    }

    crate::dispatch::finish(Ok((started, false)), "POST", "system.codebase.onboard.start")
}

/// `GET /healthz` — must match the BE field-for-field. My first version answered
/// `{ok, version, engine}`, which is a DIFFERENT shape: the BE has no `engine` key here and does
/// have `uptime_s`.
pub async fn healthz(started: std::time::Instant) -> Response {
    Json(json!({
        "ok": true,
        "version": std::env::var("npm_package_version").unwrap_or_else(|_| "0.1.0".into()),
        "uptime_s": started.elapsed().as_secs(),
        "fe_build": crate::remote::fe_build(),
    }))
    .into_response()
}

/// `GET /healthz/engine` — which engine answered. The BE hardcodes 'rust' since the JS lane was
/// retired, and this IS the rust engine, so the answer is the same either way.
pub async fn healthz_engine() -> Response {
    Json(json!({ "engine": "rust" })).into_response()
}

/// `GET /api/v1/cockpit/engines` — the vibe-pane ENGINE registry.
///
/// Engines-as-data in the BE (`focus/engines.ts`), so it is data here too. `installed` is a
/// PATH probe of the CLI binary, exactly as `binaryInstalled(e.bin)` does.
pub async fn cockpit_engines() -> Response {
    let engines: Vec<Value> = [
        ("opencode", "opencode", json!("openrouter"), "OpenCode · BYOK (OpenRouter)", false, vec!["AGENTS.md", "opencode.json"]),
        ("claude-code", "claude", Value::Null, "Claude Code · subscription", true, vec!["AGENTS.md", "CLAUDE.md"]),
        ("codex", "codex", Value::Null, "Codex · subscription", true, vec!["AGENTS.md"]),
        ("opencode-direct", "opencode", json!("openai"), "OpenCode · direct OpenAI", false, vec!["AGENTS.md", "opencode.json"]),
    ]
    .into_iter()
    .map(|(key, bin, proxy, label, subscription, ctx)| {
        let mut e = serde_json::Map::new();
        e.insert("key".into(), json!(key));
        e.insert("bin".into(), json!(bin));
        e.insert("proxy".into(), proxy);
        e.insert("label".into(), json!(label));
        // `subscription?: boolean` is optional in the DTO, so it is only present when true —
        // JSON.stringify drops an undefined field.
        if subscription {
            e.insert("subscription".into(), json!(true));
        }
        e.insert("contextFiles".into(), json!(ctx));
        e.insert("installed".into(), json!(binary_installed(bin)));
        Value::Object(e)
    })
    .collect();

    Json(json!({ "engines": engines })).into_response()
}

/// Is `bin` on PATH? Windows tries the PATHEXT suffixes the shell would.
fn binary_installed(bin: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else { return false };
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into())
            .split(';')
            .map(|s| s.to_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            if dir.join(format!("{bin}{ext}")).is_file() {
                return true;
            }
        }
    }
    false
}

/// `POST /api/v1/fe-errors` — browser crash telemetry.
///
/// Two rules the BE spells out and this keeps: severity is **never** 'error' (an unread error
/// notification halts the driver daemon), and the handler must NEVER 500 — telemetry failing is
/// not the FE's problem, so any failure answers `{ok:false}`.
pub async fn fe_errors(state: Arc<AppState>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
    let truncate = |v: Option<&Value>, fallback: &str, n: usize| -> String {
        let s = v.and_then(|x| x.as_str()).unwrap_or(fallback);
        s.chars().take(n).collect()
    };

    let message = truncate(b.get("message"), "unknown FE error", 300);
    let kind = truncate(b.get("kind"), "fe-error", 60);
    let payload = json!({
        "stack": truncate(b.get("stack"), "", 2000),
        "url": truncate(b.get("url"), "", 300),
        "ua": truncate(b.get("ua"), "", 120),
    });

    let input = json!({
        "kind": "generic",
        "severity": "warn",
        "source": "web-fe",
        "message": format!("[{kind}] {message}"),
        "payload_json": payload.to_string(),
    });

    let ok = engine_call(&state, "system.notifications.add", input, json!({ "actor": "web-fe", "source": "web-fe" }))
        .await
        .is_ok();
    (StatusCode::CREATED, Json(json!({ "ok": ok }))).into_response()
}

/// `POST /api/v1/codebase/ui/record` — record mode is interactive and long-lived (a visible
/// browser the operator drives), so the dispatch fires DETACHED and the caller gets an immediate
/// acknowledgement; the FE polls /ui/stories until the story appears. Awaiting it would turn a
/// 201 into a multi-minute request, and W1 parity caught exactly that.
pub async fn codebase_ui_record(state: Arc<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let input: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
    let ctx = crate::dispatch::attribution_ctx(&headers);
    tokio::spawn(async move {
        // `.catch(() => {})` — a detached failure is deliberately swallowed.
        let _ = engine_call(&state, "system.codebase.ui.record", input, ctx).await;
    });
    (StatusCode::CREATED, Json(json!({ "ok": true, "started": true }))).into_response()
}

/// `POST /api/v1/dispatch` — the generic dispatcher. Two branches:
///   * `action_name` present -> dispatch it with `input`, HTTP attribution
///   * otherwise -> an interviewer confirmation turn (`turn_id` + `confirmation_text`)
pub async fn generic_dispatch(state: Arc<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
    let ctx = crate::dispatch::attribution_ctx(&headers);

    if let Some(action) = b.get("action_name").and_then(|v| v.as_str()) {
        let input = b.get("input").cloned().unwrap_or_else(|| json!({}));
        return crate::dispatch::finish(
            engine_call(&state, action, input, ctx).await,
            "POST",
            action,
        );
    }

    // The fallback branch is validated in the CONTROLLER, not by a DTO: turn_id must be an
    // INTEGER >= 1 and confirmation_text a string of 1..200 chars. One combined message.
    let turn_id = b.get("turn_id");
    let confirmation_text = b.get("confirmation_text").and_then(|v| v.as_str());
    let turn_ok = turn_id
        .and_then(|v| v.as_f64())
        .is_some_and(|n| n.fract() == 0.0 && n >= 1.0);
    let text_ok = confirmation_text.is_some_and(|s| !s.is_empty() && s.chars().count() <= 200);
    if !turn_ok || !text_ok {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "message": "expected {turn_id >= 1, confirmation_text 1..200} or {action_name, input?}",
                "error": "Bad Request",
                "statusCode": 400
            })),
        )
            .into_response();
    }

    let input = json!({
        "turn_id": turn_id.cloned().unwrap_or(Value::Null),
        "confirmation_text": confirmation_text.unwrap_or_default(),
    });
    // dispatchTurn() runs attachCheckpoints on the way out, same as /turn.
    match engine_call(&state, "interviewer.dispatch", input, ctx).await {
        Ok((value, grew)) => crate::dispatch::finish(
            Ok((crate::dispatch::attach_checkpoints(&state, value).await, grew)),
            "POST",
            "interviewer.dispatch",
        ),
        err => crate::dispatch::finish(err, "POST", "interviewer.dispatch"),
    }
}

/// `GET /api/v1/settings/engine-models` — WHICH MODEL EACH TIER USES (operator 2026-09-22: "on the page of cost I
/// want an option to choose specifically the model I want to prioritise on OpenCode"). For every engine × tier: the
/// model in force, where it came from (chosen · env · built-in), and, for OpenCode, the catalogue to choose from.
pub async fn engine_models() -> Response {
    let catalog = opencode_catalog().await;
    Json(engine_models_payload(catalog)).into_response()
}
/// The settings payload, pure over the file and the seat checks (so a test reads it without a CLI). EVERY SEAT, not Claude/Codex/OpenCode (operator 2026-10-01, "Engine config also": the tier rows and the
/// jobs table listed three engines and a role was resolved against a hard-coded Claude). A role row says whether it is `auto` (the router decides per job from the benchmarks) or `pinned` to what the operator chose.
pub fn engine_models_payload(catalog: Vec<String>) -> Value {
    use crate::cli::{Engine, Tier};
    let chosen = crate::cli::chosen_models();
    let rows: Vec<Value> = Engine::all().into_iter().flat_map(|e| {
        let chosen = chosen.clone();
        [Tier::Low, Tier::Mid, Tier::High].into_iter().map(move |t| {
            let pick = chosen.get(e.key()).and_then(|x| x.get(t.key())).and_then(Value::as_str).map(str::to_string);
            let env = std::env::var(format!("ANTILOKI_TIER_MODEL_{}_{}", e.key().to_ascii_uppercase().replace('-', "_"), t.key().to_ascii_uppercase())).ok().filter(|v| !v.trim().is_empty());
            let model = crate::cli::tier_model(e, t);
            let source = if pick.is_some() { "chosen" } else if env.is_some() { "env" } else if model.is_some() { "built-in" } else { "the engine's own default" };
            json!({ "engine": e.key(), "tier": t.key(), "model": model, "source": source, "chosen": pick })
        })
    }).collect();
    // an unpinned role is resolved against the seat Heimdall plans on, never against Claude
    let seat = crate::cli::auto_engine();
    let roles: Vec<Value> = [crate::cli::Role::Planner, crate::cli::Role::Worker, crate::cli::Role::Qa].into_iter().map(|r| {
        let b = crate::cli::role_binding(r, seat);
        let pin = crate::cli::role_pin(r);
        json!({ "role": r.key(), "engine": b.engine.key(), "model": b.model, "source": b.source,
            "mode": if pin.is_some() { "pinned" } else { crate::cli::ROLE_AUTO },
            "pinned": pin.map(|(e, m)| json!({ "engine": e.map(|e| e.key()), "model": m })),
            "what": match r { crate::cli::Role::Planner => "the goal → the contract: modules, signatures, docs, acceptance", crate::cli::Role::Worker => "fills the stubs / does the ask", crate::cli::Role::Qa => "the optional gate: one isolated check per claim" } })
    }).collect();
    // THE JOBS (2026-09-22): what the router picks by, per job × engine — the measured winner and fallback, or the
    // operator's cell — with the measurement's one line beside each job
    let jobs: Vec<Value> = crate::cli::Job::ALL.into_iter().map(|j| {
        let cells: serde_json::Map<String, Value> = Engine::all().into_iter().map(|e| {
            let (w, f) = crate::cli::job_binding(j, e, None);
            (e.key().to_string(), json!({ "winner": w.model, "fallback": f.model, "source": w.source }))
        }).collect();
        json!({ "job": j.key(), "what": j.what(), "cells": cells })
    }).collect();
    // WHICH SEATS THE SCREEN MAY OFFER: the name, and whether the router can use it today (switched on, installed, signed in)
    let seats: Vec<Value> = Engine::all().into_iter().map(|e| json!({ "key": e.key(), "name": crate::cli::provider_name(e), "usable": crate::cli::engine_usable(e), "on": !crate::cli::seat_off(e) })).collect();
    json!({ "ok": true, "rows": rows, "roles": roles, "jobs": jobs, "seats": seats, "auto_engine": seat.key(), "catalog": { "opencode": catalog }, "file": crate::cli::models_file().to_string_lossy() })
}
/// `POST /api/v1/settings/engine-models/job {job, engine, winner?, fallback?}` — one cell of the jobs table; both empty
/// clears the cell back to the measurement.
pub async fn engine_models_job(body: axum::body::Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    match crate::cli::set_job_binding(b.get("job").and_then(Value::as_str).unwrap_or(""), b.get("engine").and_then(Value::as_str).unwrap_or(""), b.get("winner").and_then(Value::as_str), b.get("fallback").and_then(Value::as_str)) {
        Ok(all) => Json(json!({ "ok": true, "models": all })).into_response(),
        Err(e) => (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response(),
    }
}
/// `POST /api/v1/settings/engine-models {engine, tier, model}` — `model: null` or "" clears the choice.
pub async fn engine_models_set(body: axum::body::Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (engine, tier) = (b.get("engine").and_then(Value::as_str).unwrap_or(""), b.get("tier").and_then(Value::as_str).unwrap_or(""));
    // EVERY ENGINE, not a hardcoded three (2026-09-27: Gemini, Grok and Cursor were added and this list would have
    // silently rejected pinning a model on any of them).
    let known: Vec<&str> = crate::cli::Engine::all().iter().map(|e| e.key()).collect();
    if !known.contains(&engine) || !["low", "mid", "high"].contains(&tier) {
        return (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "ok": false,
            "error": format!("engine must be one of {} and tier low | mid | high", known.join(" | ")) }))).into_response();
    }
    match crate::cli::set_chosen_model(engine, tier, b.get("model").and_then(Value::as_str)) {
        Ok(all) => Json(json!({ "ok": true, "models": all })).into_response(),
        Err(e) => (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response(),
    }
}
/// `opencode models`, cached for the process — the catalogue the picker offers.
async fn opencode_catalog() -> Vec<String> {
    static CACHE: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    if let Some(v) = CACHE.get() { return v.clone(); }
    let out = tokio::time::timeout(std::time::Duration::from_secs(20), crate::proc::tokio_cmd("opencode").arg("models").output()).await.ok().and_then(|r| r.ok());
    let list: Vec<String> = out.map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::trim).filter(|l| l.contains('/') && !l.starts_with("mise ")).map(str::to_string).collect()).unwrap_or_default();
    let _ = CACHE.set(list.clone());
    list
}

/// `POST /api/v1/settings/engine-models/test {role?}` — DOES THIS ROLE ANSWER? (operator 2026-09-22: "test
/// connection on settings general is not working, is testing the wrong tiers"). It tested the tier providers,
/// which nothing in a run reads any more. This asks the actual binding — the engine and model the planner, the
/// worker or the QA gate would run — one tiny prompt, a short timeout, and reports what came back.
pub async fn engine_models_test(body: axum::body::Bytes) -> Response {
    use crate::cli::{ChatMessage, Role, RunOpts};
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let asked = b.get("role").and_then(Value::as_str).and_then(Role::of);
    let roles: Vec<Role> = match asked { Some(r) => vec![r], None => vec![Role::Planner, Role::Worker, Role::Qa] };
    let mut out = vec![];
    for r in roles {
        let bind = crate::cli::role_binding(r, crate::cli::auto_engine());
        let t0 = crate::cli::now_ms();
        let res = crate::cli::run_cli(
            bind.engine,
            &[ChatMessage { role: "user".into(), content: "Reply with the two letters OK and nothing else.".into() }],
            &std::env::var("ANTILOKI_SUBSTRATE_ROOT").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()), false,
            RunOpts { max_turns: Some(1), timeout_ms: Some(90_000), model: bind.model.clone(), ..Default::default() },
            None,
        ).await;
        let ms = crate::cli::now_ms() - t0;
        out.push(match res {
            Ok(o) => {
                let said = o.parsed.content.trim().chars().take(80).collect::<String>();
                let ok = !said.is_empty();
                json!({ "role": r.key(), "engine": bind.engine.key(), "model": bind.model, "ok": ok, "ms": ms,
                    "said": said, "cost": o.trace.cost,
                    "error": if ok { Value::Null } else { json!("the engine answered with nothing — the model name may be wrong for this account") } })
            }
            Err(e) => json!({ "role": r.key(), "engine": bind.engine.key(), "model": bind.model, "ok": false, "ms": ms, "error": e }),
        });
    }
    Json(json!({ "ok": out.iter().all(|r| r.get("ok").and_then(Value::as_bool).unwrap_or(false)), "results": out })).into_response()
}

/// `POST /api/v1/onboarding/seat-check {seat}` - THE FIRST-RUN CHECK (operator 2026-10-11: "before any use all providers CLIs must be tested and gather the weeks usage of all on antiloki,
/// so the user already starts seeing it at the switcher"). Per seat: is the CLI there, is it signed in, does ONE tiny real call come back through the same runner every job uses (so a flag
/// that makes a CLI print nothing is found now, not on the first real task), and what does the seat say about this week. Claude's plan windows come from the stream of that very call
/// (`meter_root`), Codex's from the session file the call just wrote, Cursor's from its `/usage` panel, and the seats that publish no window (Gemini, Grok) say so.
/// One seat per call: the screen runs them one after another and shows each as it lands. Never spends more than one tiny prompt.
pub async fn seat_check(state: Arc<AppState>, body: axum::body::Bytes) -> Response {
    use crate::cli::{ChatMessage, Engine, RunOpts, Tier};
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let key = b.get("seat").and_then(Value::as_str).unwrap_or("");
    let Some(engine) = crate::cli::engine_of(&json!(key)) else {
        return (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": format!("`{key}` is not a seat") }))).into_response();
    };
    let root = crate::agents_http::active_root(&state).await;
    let cwd = if root.is_empty() { std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default() } else { root.clone() };
    let fail = |stage: &str, error: String| Json(json!({ "ok": false, "seat": engine.key(), "stage": stage, "error": error, "windows": [] })).into_response();
    if engine.bin_present().is_none() { return fail("install", format!("`{}` is not installed", engine.bin())); }
    if crate::cli::engine_seat(engine) == Some(false) { return fail("signin", format!("`{}` is installed but not signed in", engine.bin())); }
    let t0 = crate::cli::now_ms();
    let res = crate::cli::run_cli(
        engine,
        &[ChatMessage { role: "user".into(), content: "Reply with the two letters OK and nothing else.".into() }],
        &cwd, false,
        RunOpts { max_turns: Some(1), timeout_ms: Some(120_000), tier: Some(Tier::Low), meter_root: if engine == Engine::ClaudeCode { Some(cwd.clone()) } else { None }, ..Default::default() },
        None,
    ).await;
    let ms = crate::cli::now_ms() - t0;
    let (ok, said, error, cost, model) = match res {
        Ok(o) => { let said = o.parsed.content.trim().chars().take(80).collect::<String>(); let ok = !said.is_empty() && o.parsed.error.is_none();
                   let err = if ok { None } else { Some(o.parsed.error.unwrap_or_else(|| "it answered with nothing".into())) }; (ok, said, err, o.trace.cost, o.trace.model.clone()) }
        Err(e) => (false, String::new(), Some(e), None, None),
    };
    // what the seat says about this week. Cursor's panel is read now (up to ~35 s, off the async threads); the others were written by the call above or by earlier use.
    if engine == Engine::Cursor { let _ = tokio::task::spawn_blocking(crate::cursor_usage::refresh_now).await; }
    let house = if engine == Engine::ClaudeCode { "claude" } else { engine.key() };
    let windows: Vec<Value> = crate::agents_http::account_windows(&cwd).into_iter().filter(|w| w.get("house").and_then(Value::as_str) == Some(house)).collect();
    let note = match engine {
        Engine::Gemini | Engine::Grok => "this seat publishes no usage figure: antiloki counts what it sends from now on, and you can declare a weekly allowance in Settings > Seats",
        _ if windows.is_empty() && ok => "the call worked, but the seat reported no usage window (yet)",
        _ => "",
    };
    Json(json!({ "ok": ok, "seat": engine.key(), "stage": if ok { "done" } else if error.as_deref().map_or(false, crate::signin::looks_signed_out) { "signin" } else { "test" }, "ms": ms, "said": said, "cost": cost, "model": model, "error": error, "windows": windows, "note": note })).into_response()
}

/// `POST /api/v1/settings/engine-models/role {role, engine?, model?}` — THE ROLE BINDINGS (PLAN-THE-DIAL): what the
/// planner, the worker and the QA gate each run on. Both fields empty - or `engine: "auto"` - hands the role back to the router (the default); anything else is a pin.
pub async fn engine_models_role(body: axum::body::Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    match crate::cli::set_role_binding(b.get("role").and_then(Value::as_str).unwrap_or(""), b.get("engine").and_then(Value::as_str), b.get("model").and_then(Value::as_str)) {
        Ok(all) => Json(json!({ "ok": true, "models": all })).into_response(),
        Err(e) => (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response(),
    }
}

/// `GET /api/v1/settings/providers` — THE TWO SWITCHES AND WHERE EVERY SEAT STANDS (operator 2026-09-27: "add on
/// settings an option to make Zen reachable or not … and by default no reachable" · "a button on settings to activate
/// or deactivate Chinese models. Don't make Chinese models obligatory").
///
/// The switches are two and not one because they answer different questions: `zen` is a BILLING PATH (OpenCode's
/// metered credit, which emptied mid-bench and returned HTTP 402 on twelve consecutive runs), `chinese_models` is a
/// MODEL FAMILY that must stay refused however it is reached. Both default OFF.
///
/// The per-engine rows are what makes the screen usable rather than merely truthful: an operator who cannot see that
/// a seat is installed-but-not-signed-in has no way to know why the router never picks it.
pub async fn providers() -> Response {
    use crate::cli::Engine;
    let rows: Vec<Value> = Engine::all().iter().map(|&e| {
        let seat = crate::cli::engine_seat(e);
        json!({
            "key": e.key(),
            "name": crate::cli::provider_name(e),
            "binaries": e.bins(),
            "binary": e.bin_present(),
            "installed": e.bin_present().is_some(),
            "signed_in": seat,
            "usable": crate::cli::engine_usable(e),
            "metered_gateway": e.is_metered_gateway(),
            // why the router will not use it, in the words the screen should show
            "blocked": crate::cli::model_blocked(e, None),
            // the operator's own switch for this whole seat (Claude's subscription ending is the case that made it)
            "on": !crate::cli::seat_off(e),
            // every model of the seat the router knows, each with its own switch (operator 2026-10-07): a model switched off is never chosen by any router path
            "models": crate::cli::known_models(e).into_iter().map(|m| json!({ "model": m, "on": !crate::cli::model_off(e, &m) })).collect::<Vec<Value>>(),
            // …AND HOW TO FIX IT (2026-09-28). "installed, not signed in" is a diagnosis, not a repair: the
            // operator's own rule is that every missing piece shows the EXACT command. Both are strings to
            // SHOW — install and sign-in are always the user's own act, in their own terminal, because a
            // credential must never pass through the agent.
            "install": crate::engines::install_for(e.key(), std::env::consts::OS),
            // what has to be on the machine before the install line works (Node.js for the npm ones, a Google account for the free Gemini tier)
            "install_cmd": crate::engines::install_cmd_for(e.key(), std::env::consts::OS),
            "shell": crate::engines::shell_name(std::env::consts::OS),
            "needs": crate::engines::needs_for(e.key(), std::env::consts::OS),
            "os": std::env::consts::OS,
            "signin": crate::engines::signin_for_found(e.key(), crate::proc::which(e.bin()).map(|p| p.to_string_lossy().to_string()).as_deref()),
            // the weekly allowance the operator declared for this seat (dollars at list price), when it publishes no window of its own
            "week_usd": crate::cli::allowances().into_iter().find(|(k, _)| k == e.key()).map(|(_, v)| v),
        })
    }).collect();
    Json(json!({
        "ok": true,
        "switches": {
            "zen": { "on": crate::cli::zen_enabled(), "default": false,
                     "label": "OpenCode Zen (metered credit)",
                     "note": "Zen bills a credit balance, not your subscription. Off unless you hold Zen credit." },
            "chinese_models": { "on": crate::cli::chinese_models_enabled(), "default": false,
                     "label": "Chinese models (DeepSeek, Qwen, Kimi, GLM, MiniMax)",
                     "note": "Optional, never required. Off by default; they are measured and they work, but they are \
                              not on the subscription seats antiloki merges." },
        },
        "engines": rows,
        // HEIMDALL'S OWN SEAT (operator 2026-09-27: "we should have a main provider for heimdall. On settings as well
        // to select"): which seat plans, verifies, joins and judges — "pane" keeps the pane's own engine
        "heimdall": {
            "provider": crate::cli::heimdall_provider(),
            "effective": crate::cli::heimdall_engine(None).map(|e| e.key()),
            "options": Engine::all().iter().map(|&e| json!({ "key": e.key(), "name": crate::cli::provider_name(e), "usable": crate::cli::engine_usable(e) && crate::cli::model_blocked(e, None).is_none() })).collect::<Vec<_>>(),
            "label": "Heimdall plans with",
            "note": "The seat the router thinks on — planning, verifying, joining and judging. \"the pane's own engine\" follows whatever the agent is on; a seat that is blocked or signed out is never used, the first usable one plans instead.",
        },
    })).into_response()
}

/// `GET /api/v1/settings/forge-tokens` - which forges antiloki can file pull requests on, and WHERE each one's token comes from right now. Never the token itself.
fn forge_tokens_payload() -> Result<Value, String> {
    use crate::forge::{Kind, find_token};
    let mut hosts: Vec<String> = vec!["github.com".into(), "gitlab.com".into(), "codeberg.org".into()];
    for h in crate::forge::stored_hosts() { if !hosts.contains(&h) { hosts.push(h); } }
    let gh = crate::forge::gh_state();
    let rows: Vec<Value> = hosts.iter().map(|h| {
        let kind = crate::forge::detect(&format!("https://{h}/x/y"), &json!({})).0;
        let kind = if kind == Kind::Patch { Kind::GitHub } else { kind };
        let active = find_token(kind, h).ok().map(|t| t.from);
        json!({ "host": h, "kind": kind.key(), "active_from": active, "stored": crate::forge::token_file(h).map_or(false, |p| p.is_file()) })
    }).collect();
    Ok(json!({ "ok": true, "hosts": rows, "gh": { "installed": gh.0, "account": gh.1 }, "where": "~/.antiloki/tokens/<host> (owner-only)",
            "advice": "GitHub: a fine-grained token limited to the repositories you use, with Contents and Pull requests: read and write." }))
}
/// Filled once before the port opens, so the first settings page after a restart is as fast as the next.
pub fn forge_tokens_warm() { std::thread::spawn(|| { let _ = crate::swr::get("forge:tokens", std::time::Duration::from_secs(20), forge_tokens_payload); }); }
pub async fn forge_tokens() -> Response {
    let out = tokio::task::spawn_blocking(|| crate::swr::get("forge:tokens", std::time::Duration::from_secs(20), forge_tokens_payload))
        .await.unwrap_or_else(|e| Err(e.to_string())).unwrap_or_else(|e| json!({ "ok": false, "error": e }));
    Json(out).into_response()
}
/// `POST /api/v1/settings/forge-tokens {host, token}` saves (GitHub tokens are verified first and a wrong one is refused); `{host, remove: true}` forgets it.
pub async fn forge_tokens_set(body: axum::body::Bytes) -> Response {
    crate::swr::forget("forge:");   // what was saved or removed shows at once
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let host = b.get("host").and_then(Value::as_str).unwrap_or("").trim().to_ascii_lowercase();
    let bad = |m: String| (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": m }))).into_response();
    if !crate::forge::host_ok(&host) { return bad(format!("{host:?} is not a host name")) }
    if b.get("remove").and_then(Value::as_bool) == Some(true) {
        return match crate::forge::remove_token(&host) { Ok(had) => Json(json!({ "ok": true, "removed": had, "host": host })).into_response(), Err(e) => bad(e) };
    }
    let token = match crate::forge::token_shape_ok(b.get("token").and_then(Value::as_str).unwrap_or("")) { Ok(t) => t, Err(e) => return bad(e) };
    let h2 = host.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let is_github = crate::forge::detect(&format!("https://{h2}/x/y"), &json!({})).0 == crate::forge::Kind::GitHub;
        let mut note = None; let mut login = None;
        if is_github {
            match crate::forge::verify_github_token(&h2, &token) {
                Ok(l) => login = Some(l),
                Err(e) if crate::forge::verification_rejects(&e) => return Err(e),
                Err(e) => note = Some(format!("saved, but {e}")),
            }
        }
        let path = crate::forge::save_token(&h2, &token)?;
        Ok(json!({ "ok": true, "host": h2, "login": login, "note": note, "path": path.to_string_lossy() }))
    }).await.unwrap_or_else(|e| Err(e.to_string()));
    match out { Ok(v) => Json(v).into_response(), Err(e) => bad(e) }
}

/// `POST /api/v1/settings/providers {name, on}` — flip one switch. Only the two names are accepted.
pub async fn providers_set(body: axum::body::Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let name = b.get("name").and_then(Value::as_str).unwrap_or("");
    // `{name: "heimdall", provider: "codex" | "pane" | …}` — the seat Heimdall plans on
    if name == "heimdall" {
        let key = b.get("provider").and_then(Value::as_str).unwrap_or("");
        return match crate::cli::set_heimdall_provider(key) {
            Ok(_) => Json(json!({ "ok": true, "name": "heimdall", "provider": crate::cli::heimdall_provider(), "effective": crate::cli::heimdall_engine(None).map(|e| e.key()) })).into_response(),
            Err(e) => (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response(),
        };
    }
    // `{name: "<engine key>", used_pct: 90}` - the ONE number to read off the seat's own dashboard: antiloki turns it, with its own 7-day spend on that seat, into the weekly allowance
    if let Some(pct) = b.get("used_pct").and_then(Value::as_f64) {
        let Some(e) = crate::cli::engine_of(&json!(name)) else {
            return (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": format!("unknown seat: {name}") }))).into_response();
        };
        let house = crate::fuel::house_of(e.key());
        let spent: f64 = crate::audit::calls_since_hours(7 * 24).iter().filter(|r| r.get("house").and_then(Value::as_str) == Some(house.as_str())).filter_map(|r| r.get("cost").and_then(Value::as_f64)).sum();
        return match crate::fuel::allowance_from_pct(spent, pct) {
            Some(usd) => match crate::cli::set_weekly_allowance(name, Some(usd)) {
                Ok(_) => Json(json!({ "ok": true, "name": name, "week_usd": usd, "spent_usd": spent, "used_pct": pct })).into_response(),
                Err(err) => (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": err }))).into_response(),
            },
            None => (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": if spent > 0.0 { "the percentage must be between 0 and 100".to_string() } else { format!("antiloki has not sent {name} anything in the last 7 days, so there is nothing to calibrate on") } }))).into_response(),
        };
    }
    // `{name: "<engine key>", week_usd: 25 | null}` - the weekly allowance of a seat that publishes no window (null or <= 0 clears it)
    if b.get("week_usd").is_some() {
        let usd = b.get("week_usd").and_then(Value::as_f64);
        return match crate::cli::set_weekly_allowance(name, usd) {
            Ok(_) => Json(json!({ "ok": true, "name": name, "week_usd": usd.filter(|x| *x > 0.0) })).into_response(),
            Err(e) => (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response(),
        };
    }
    let on = b.get("on").and_then(Value::as_bool);
    let Some(on) = on else {
        return (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "on must be true or false" }))).into_response();
    };
    // `{name: "<engine key>", model: "<model>", on}` - ONE MODEL's switch (operator 2026-10-07)
    if let Some(model) = b.get("model").and_then(Value::as_str) {
        let Some(e) = crate::cli::engine_of(&json!(name)) else {
            return (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": format!("unknown seat: {name}") }))).into_response();
        };
        return match crate::cli::set_model_off(e, model, !on) {
            Ok(_) => Json(json!({ "ok": true, "name": name, "model": model, "on": on })).into_response(),
            Err(err) => (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": err }))).into_response(),
        };
    }
    match crate::cli::set_provider_flag(name, on) {
        Ok(_) => Json(json!({ "ok": true, "name": name, "on": on,
                              "zen": crate::cli::zen_enabled(), "chinese_models": crate::cli::chinese_models_enabled() })).into_response(),
        Err(e) => (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response(),
    }
}
