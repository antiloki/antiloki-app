//! byok-local-models-list.rs — twin of skills/system/byok-local-models-list.mjs. NAMES ARE
//! CONTRACT: default `byokLocalModelsList`. Lists the models a local Ollama server has PULLED
//! (a pure read of `GET {host}/api/tags`) so the Cost Center can offer a dropdown instead of a
//! free-text model name.
//!
//! LANE A (bespoke-native network adapter): the real `fetch(url, { signal })` round-trip against
//! the user's local Ollama server is the DEFERRED boundary — genuinely non-deterministic (a live
//! server's response, or its absence), not JSON-fixturable, unconvertible IN PRINCIPLE. Everything
//! DETERMINISTIC around it is ported faithfully as pure fns and IS what this file's tests exercise:
//! - `resolve_endpoint(input)`: JS `override ?? resolveHost()` — an explicit non-blank
//!   `input.endpoint` (trimmed, trailing slashes stripped) wins, else the shared
//!   `external__helpers_ollama_host::resolveHost()` twin.
//! - `build_url(host)`: JS `` `${host}/api/tags` ``.
//! - `parse_models(body)`: JS `.map(...).filter((m) => m.name)` over `body?.models` — maps each
//!   Ollama tag entry to `{ name, parameter_size, quantization, family, capabilities, thinking }`
//!   and drops entries with no resolvable name.
//! - `http_error_message(host, status)` / `network_error_message(host, is_timeout, message)`: the
//!   two JS fail-soft error-string branches (`!r.ok` and the `catch` block), so both are covered
//!   even though this twin never reaches a real socket.
//!
//! `byokLocalModelsList` (the default export) is the PARITY-DEFERRED stub: it resolves the
//! endpoint + builds the request URL (proving that deterministic surface), then stops before the
//! network hop — same convention as deepseek-call.rs's `deepseekCall` / ai-call.rs's `aiCall`. No
//! fixture, no frontier entry — BESPOKE-NATIVE TESTING only, via `#[cfg(test)]` below. Unlike most
//! provider adapters the JS never throws (it always returns `{ host, models, error? }`), but since
//! there is no real HTTP client wired in this crate the deferred stub still surfaces as `Err` —
//! callers that need the JS's fail-soft `Ok({ models: [], error })` shape must supply their own
//! fetch client and call `parse_models`/`http_error_message`/`network_error_message` directly.

use serde_json::{json, Value};

/// JS: `str.replace(/\/+$/, '')` — strip one-or-more trailing slashes.
fn strip_trailing_slashes(s: &str) -> &str {
    s.trim_end_matches('/')
}

/// JS: `const override = typeof input?.endpoint === 'string' && input.endpoint.trim()
/// ? input.endpoint.trim().replace(/\/+$/, '') : null; const host = override ?? resolveHost();`
pub fn resolve_endpoint(input: &Value) -> String {
    let override_host = input
        .get("endpoint")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(strip_trailing_slashes);
    match override_host {
        Some(h) => h.to_string(),
        None => crate::external__helpers_ollama_host::resolveHost(),
    }
}

/// JS: `` `${host}/api/tags` ``.
pub fn build_url(host: &str) -> String {
    format!("{host}/api/tags")
}

/// JS: `Ollama at ${host} returned HTTP ${r.status}.` (the `!r.ok` branch).
pub fn http_error_message(host: &str, status: i64) -> String {
    format!("Ollama at {host} returned HTTP {status}.")
}

/// JS: the `catch (e)` branch —
/// `` `Could not reach Ollama at ${host} (${e?.name === 'AbortError' ? 'timed out' : e?.message ?? String(e)}). Is \`ollama serve\` running, and is the endpoint right?` ``.
pub fn network_error_message(host: &str, is_timeout: bool, message: &str) -> String {
    let reason = if is_timeout { "timed out" } else { message };
    format!(
        "Could not reach Ollama at {host} ({reason}). Is `ollama serve` running, and is the endpoint right?"
    )
}

/// JS: `(Array.isArray(j?.models) ? j.models : []).map((m) => { ... }).filter((m) => m.name);`
/// `body` is the already-parsed JSON response body (`j` in the JS).
pub fn parse_models(body: &Value) -> Vec<Value> {
    let models = body.get("models").and_then(Value::as_array).cloned().unwrap_or_default();
    models
        .into_iter()
        .filter_map(|m| {
            // JS: `m?.name ?? m?.model ?? null`
            let name = m
                .get("name")
                .filter(|v| !v.is_null())
                .or_else(|| m.get("model").filter(|v| !v.is_null()))
                .cloned()
                .unwrap_or(Value::Null);
            // JS: `.filter((m) => m.name)` — JS truthiness drops null/undefined AND an empty string.
            if name.is_null() || name.as_str() == Some("") {
                return None;
            }
            let details = m.get("details").cloned().unwrap_or(Value::Null);
            let parameter_size = details.get("parameter_size").cloned().unwrap_or(Value::Null);
            let quantization = details.get("quantization_level").cloned().unwrap_or(Value::Null);
            let family = details.get("family").cloned().unwrap_or(Value::Null);
            let caps = m.get("capabilities").and_then(Value::as_array).cloned().unwrap_or_default();
            let thinking = caps.iter().any(|c| c.as_str() == Some("thinking"));
            Some(json!({
                "name": name,
                "parameter_size": parameter_size,
                "quantization": quantization,
                "family": family,
                "capabilities": Value::Array(caps),
                "thinking": thinking,
            }))
        })
        .collect()
}

/// PARITY-DEFERRED (whole function — same convention as deepseek-call.rs's `deepseekCall`):
/// resolves the endpoint + builds the request URL via `resolve_endpoint`/`build_url` (proving that
/// deterministic surface), then stops before the network hop. Never dispatched by conform-db (no
/// frontier entry, no fixture) — exists so the module compiles and any future caller resolves by
/// name.
pub fn byokLocalModelsList(input: &Value) -> Result<Value, String> {
    let host = resolve_endpoint(input);
    let _url = build_url(&host);
    Err(format!(
        "byok-local-models-list twin: byokLocalModelsList (the default export) is PARITY-DEFERRED — see the module doc. It dispatches a real HTTP GET to the local Ollama server ({host}/api/tags), which is genuinely non-deterministic and not fixturable. Use resolve_endpoint/build_url/parse_models/http_error_message/network_error_message for the deterministic surface."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_endpoint_uses_trimmed_slash_stripped_override() {
        let host = resolve_endpoint(&json!({ "endpoint": "  http://box:9000//  " }));
        assert_eq!(host, "http://box:9000");
    }

    #[test]
    fn resolve_endpoint_falls_back_to_resolve_host_when_blank_or_missing() {
        // SAFETY: test-only env mutation of a process-unique var; no concurrent readers in this test.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("OLLAMA_HOST", "http://envhost:11111");
        }
        assert_eq!(resolve_endpoint(&json!({ "endpoint": "   " })), "http://envhost:11111");
        assert_eq!(resolve_endpoint(&json!({})), "http://envhost:11111");
        assert_eq!(resolve_endpoint(&Value::Null), "http://envhost:11111");
        unsafe {
            std::env::remove_var("OLLAMA_HOST");
        }
    }

    #[test]
    fn build_url_appends_api_tags() {
        assert_eq!(build_url("http://localhost:11434"), "http://localhost:11434/api/tags");
    }

    #[test]
    fn http_error_message_matches_js_template() {
        assert_eq!(
            http_error_message("http://localhost:11434", 500),
            "Ollama at http://localhost:11434 returned HTTP 500."
        );
    }

    #[test]
    fn network_error_message_timeout_branch() {
        let msg = network_error_message("http://localhost:11434", true, "irrelevant");
        assert_eq!(
            msg,
            "Could not reach Ollama at http://localhost:11434 (timed out). Is `ollama serve` running, and is the endpoint right?"
        );
    }

    #[test]
    fn network_error_message_non_timeout_branch() {
        let msg = network_error_message("http://localhost:11434", false, "ECONNREFUSED");
        assert_eq!(
            msg,
            "Could not reach Ollama at http://localhost:11434 (ECONNREFUSED). Is `ollama serve` running, and is the endpoint right?"
        );
    }

    #[test]
    fn parse_models_maps_fields_and_thinking_flag() {
        let body = json!({
            "models": [
                {
                    "name": "qwen2.5:14b",
                    "details": { "parameter_size": "14B", "quantization_level": "Q4_K_M", "family": "qwen2" },
                    "capabilities": ["completion", "thinking"],
                },
                { "model": "llama3:8b" },
            ]
        });
        let out = parse_models(&body);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["name"], json!("qwen2.5:14b"));
        assert_eq!(out[0]["parameter_size"], json!("14B"));
        assert_eq!(out[0]["quantization"], json!("Q4_K_M"));
        assert_eq!(out[0]["family"], json!("qwen2"));
        assert_eq!(out[0]["capabilities"], json!(["completion", "thinking"]));
        assert_eq!(out[0]["thinking"], json!(true));
        // falls back to `model` when `name` is absent; no details/capabilities → nulls/empty/false
        assert_eq!(out[1]["name"], json!("llama3:8b"));
        assert_eq!(out[1]["parameter_size"], Value::Null);
        assert_eq!(out[1]["capabilities"], json!([]));
        assert_eq!(out[1]["thinking"], json!(false));
    }

    #[test]
    fn parse_models_filters_out_nameless_entries() {
        let body = json!({ "models": [{ "details": {} }, { "name": "" }] });
        // JS: `.filter((m) => m.name)` — an empty string is falsy in JS, so it is dropped too.
        let out = parse_models(&body);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn parse_models_handles_missing_or_non_array_models() {
        assert!(parse_models(&json!({})).is_empty());
        assert!(parse_models(&json!({ "models": "not-an-array" })).is_empty());
    }

    #[test]
    fn byok_local_models_list_is_parity_deferred() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("OLLAMA_HOST", "http://localhost:11434");
        }
        let err = byokLocalModelsList(&json!({})).unwrap_err();
        assert!(err.contains("PARITY-DEFERRED"), "{err}");
        assert!(err.contains("http://localhost:11434/api/tags"), "{err}");
        unsafe {
            std::env::remove_var("OLLAMA_HOST");
        }
    }
}
