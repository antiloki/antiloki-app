//! Binary file responses — the three audio GETs.
//!
//! These are the only routes that answer with bytes instead of JSON, so they cannot go through
//! the generated handler (which always emits `Json(..)`). Each resolves a path via the engine,
//! then streams the file.
//!
//! The BE pipes a read stream; here the file is read into memory. Deliberate: these are single
//! narration/TTS clips a browser fetches whole, and `Content-Length` is already required by the
//! notebook route — streaming would add complexity for no user-visible gain. If large media ever
//! lands here, swap the body for a `ReaderStream`.

use axum::{
    body::Body,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::dispatch::{engine_call, AppState};

/// Serve `abs` with the given content type, or the caller's 404 body when it is missing.
fn serve_file(abs: &Path, content_type: &str, extra_no_store: bool, missing: Response) -> Response {
    let Ok(bytes) = std::fs::read(abs) else { return missing };
    let mut res = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, bytes.len().to_string());
    if extra_no_store {
        res = res.header(header::CACHE_CONTROL, "no-store");
    }
    res.body(Body::from(bytes)).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn json_status(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

/// `GET /api/v1/artefacts/audio/:file`
pub async fn artefact_audio(
    state: Arc<AppState>,
    headers: HeaderMap,
    file: String,
) -> Response {
    let ctx = crate::dispatch::attribution_ctx(&headers);
    // A dispatch FAILURE propagates as the engine envelope (the BE lets the exception reach
    // EngineErrorFilter). Only a SUCCESSFUL dispatch with no usable abs_path gets the handler's
    // own `{ok:false,...}` 404. Swallowing the error collapsed the two into one shape.
    let out = match engine_call(&state, "system.artefacts.manage", json!({ "op": "get", "file": file }), ctx).await {
        Ok((v, _)) => v,
        Err(e) => return crate::dispatch::finish(Err(e), "GET", "system.artefacts.manage"),
    };

    let missing = json_status(StatusCode::NOT_FOUND, json!({ "ok": false, "error": "artefact not found" }));
    match out.get("abs_path").and_then(|v| v.as_str()) {
        Some(abs) if Path::new(abs).exists() => serve_file(Path::new(abs), "audio/wav", false, missing),
        _ => missing,
    }
}

/// `GET /api/v1/narration/audio/:renderId`
pub async fn narration_audio(
    state: Arc<AppState>,
    headers: HeaderMap,
    render_id: String,
) -> Response {
    let ctx = crate::dispatch::attribution_ctx(&headers);
    // `Number(renderId)` in the controller — junk becomes NaN, which JSON.stringify renders null.
    let id: Value = render_id
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|f| !f.is_nan())
        .map(|f| if f.fract() == 0.0 { Value::from(f as i64) } else { Value::from(f) })
        .unwrap_or(Value::Null);

    let out = match engine_call(
        &state,
        "system.narration.render",
        json!({ "op": "render_get", "render_id": id }),
        ctx,
    )
    .await
    {
        Ok((v, _)) => v,
        Err(e) => return crate::dispatch::finish(Err(e), "GET", "system.narration.render"),
    };

    let missing = json_status(StatusCode::NOT_FOUND, json!({ "ok": false, "error": "render not found" }));
    match out.get("abs_path").and_then(|v| v.as_str()) {
        Some(abs) if Path::new(abs).exists() => serve_file(Path::new(abs), "audio/wav", false, missing),
        _ => missing,
    }
}

/// `GET /api/v1/notebook/audio/:id`
///
/// Three distinct failure shapes, all Nest exceptions with a STRING message:
///   bad id            -> 400 "bad item id"
///   no file on item   -> 404 "no audio on this item"
///   unresolvable/gone -> 404 "artifact unavailable" / "audio file missing on disk"
pub async fn notebook_audio(state: Arc<AppState>, id: String) -> Response {
    let item_id = id.trim().parse::<f64>().ok().filter(|f| !f.is_nan());
    let valid = item_id.is_some_and(|f| f.fract() == 0.0 && f > 0.0);
    if !valid {
        return json_status(
            StatusCode::BAD_REQUEST,
            json!({ "message": "bad item id", "error": "Bad Request", "statusCode": 400 }),
        );
    }
    let item_id = item_id.unwrap() as i64;

    let ctx = json!({ "actor": "web-fe", "source": "web-fe" });
    let meta = match engine_call(&state, "system.notebook.item.meta", json!({ "id": item_id }), ctx).await {
        Ok((v, _)) => v,
        Err(e) => return crate::dispatch::finish(Err(e), "GET", "system.notebook.item.meta"),
    };

    let Some(file) = meta.get("file").and_then(|v| v.as_str()) else {
        return json_status(
            StatusCode::NOT_FOUND,
            json!({ "message": "no audio on this item", "error": "Not Found", "statusCode": 404 }),
        );
    };

    let Some(abs) = artifact_file(file) else {
        return json_status(
            StatusCode::NOT_FOUND,
            json!({ "message": "artifact unavailable", "error": "Not Found", "statusCode": 404 }),
        );
    };
    if !abs.exists() {
        return json_status(
            StatusCode::NOT_FOUND,
            json!({ "message": "audio file missing on disk", "error": "Not Found", "statusCode": 404 }),
        );
    }

    let mime = meta.get("mime").and_then(|v| v.as_str()).unwrap_or("audio/mpeg");
    let missing = json_status(
        StatusCode::NOT_FOUND,
        json!({ "message": "audio file missing on disk", "error": "Not Found", "statusCode": 404 }),
    );
    serve_file(&abs, mime, true, missing)
}

/// Port of `EngineService.artifactFile(rel)` — resolve under the artifacts root and REFUSE a path
/// that escapes it. The traversal guard is the whole point; without it `..%2f..%2f` reads any file
/// the process can.
fn artifact_file(rel: &str) -> Option<PathBuf> {
    let root: PathBuf = antiloki_rs::engine_roots::artifacts_dir();
    let root = root.canonicalize().unwrap_or(root);
    let abs = root.join(rel);
    let abs = abs.canonicalize().unwrap_or(abs);
    if abs == root || abs.starts_with(&root) {
        Some(abs)
    } else {
        None
    }
}
