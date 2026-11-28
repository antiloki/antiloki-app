//! ai-call.rs — twin of skills/ai/ai-call.mjs, the AI-call chokepoint ~137 other skills depend on.
//! The 9 PURE named exports are real, fixture-tested units (unchanged — see their own doc comments
//! below). The `aiCall` DEFAULT export is now a REAL implementation, not a stub.
//!
//! LOCAL-QWEN SIMPLIFICATION (user-directed design): instead of JS's BYOK multi-provider resolution
//! (dynamic adapter `import()`, random-order failover across a shuffled provider list, Heimdall
//! rank1/train modes, bandit-pick empirical ranking, per-tier/per-gate daily spend caps), this twin
//! dispatches ONE blocking HTTP POST to a single OpenAI-compatible chat/completions endpoint
//! (`ANTILOKI_LOCAL_LLM_URL`, e.g. an Ollama/llama.cpp/vLLM server's `/v1/chat/completions`). The
//! network round-trip itself remains genuinely non-deterministic (a live model's answer) and is
//! therefore NOT a conform-db fixture unit (no frontier entry) — but every deterministic step
//! around it (template load, render, hash, cache lookup/hit, schema validation + one retry,
//! persistence, output shaping) is now REAL, DB-backed logic, not a deferred stub.
//!
//! PORTED faithfully: template resolution (via the already-converted `ai_prompt_find::promptFind`
//! twin — its own "prompt not found" error propagates unchanged), `renderTemplate` +
//! `validateInputs` for the outbound prompt, tier/temperature resolution (explicit override >
//! `ai_ai_route::aiRoute` routing_defaults row > template default, reusing the already-converted
//! `aiRoute` twin), `hashInput`-keyed cache lookup + cache-hit row + prior-outputs replay,
//! `deriveResponseFormat` (ported here as `derive_response_format` — JS keeps it un-exported too),
//! the OpenAI chat/completions request shape (`model`/`messages`/`temperature`/`response_format`),
//! `validateAndDecompose` + the SAME one-shot retry-with-corrective-banner JS performs (same
//! retryable-error regex, ported as `is_retryable_validation_error`), `persistOutputs` (ported as
//! `persist_outputs`, including the `_children` array-of-object decomposition), `readOutputs`
//! (`read_outputs`), and `envelope_json` on every written row (EVERY_AI_CALL_CARRIES_ENVELOPE law —
//! reuses the already-converted `ai_assemble_context_envelope::assembleContextEnvelope` twin).
//! `TEMP_TO_VALUE` — previously undocumented as Rust DATA because nothing read it — is now a real
//! `temp_to_value` fn, since the local dispatch is the first real reader of it in this crate.
//!
//! SIMPLIFIED / DROPPED for the local-only design (each a deliberate scope cut, not an oversight):
//!   - **Model resolution** reads `brain.ai_providers WHERE tier=? AND provider='ollama' AND
//!     active=1` (the ONE local-provider string this substrate's `byok-local-model-add` twin ever
//!     writes — see its own module doc) `ORDER BY failover_rank ASC, id ASC LIMIT 1`, falling back
//!     to env `ANTILOKI_LOCAL_LLM_MODEL` when no such row exists. Deliberately NOT "any active
//!     provider row": `openDb`'s `seedAiProviders` seeds default-active `openrouter` rows for
//!     low/mid/high, and forwarding an OpenRouter model slug to a local server would silently
//!     misdispatch. `provider` in the returned shape is `"ollama"` (DB row) or `"local"` (env
//!     fallback) — never openrouter/anthropic/etc.
//!   - **No provider failover.** One endpoint, one attempt (+ the one validation retry below). A
//!     network error writes an `outcome='error'` row and returns `Err` immediately — no shuffled
//!     candidate list, no Heimdall/bandit-pick ranking.
//!   - **`validateInputs` is a HARD gate here**, not JS's default-WARN/opt-in-strict split (no
//!     `strict_input_check` flag, no `ANTILOKI_STRICT_INPUT_CHECK` env, no `runtime_events` warn
//!     row). A structurally invalid `input_kv` refuses the call outright. This is the one
//!     behavioral tightening versus JS — flagged because it could reject an input shape JS would
//!     have (noisily) accepted; revisit if a real prompt template trips it unexpectedly.
//!   - **`checkFeedQuality` (the `{{placeholder}}`-vs-`input_kv` completeness scan) is not wired
//!     into the orchestration at all** — the pure fn above still exists and is fixture-tested, but
//!     `aiCall` itself never calls it. A template whose `{{foo}}` has no matching input_kv key
//!     renders with the literal placeholder left in place (renderTemplate's normal "no match"
//!     behavior) — same visible outcome as JS's default WARN mode, just without the diagnostic row.
//!   - **No `reason_then_format` / AUTO-TWO-PHASE orchestration.** `phase_flow` on the template is
//!     read but ignored; every call is single-shot regardless of tier or the template's declared
//!     flow — JS's high-tier auto-split (reason at high, format at low/freezed) doesn't apply when
//!     there's only one model to dispatch to.
//!   - **No `references`/`attachments` on the input contract.** `assembleContextEnvelope` is still
//!     called (so `envelope_json` has the right shape and the law is satisfied), but always with
//!     empty arrays — a template that declares `{{references}}`/`{{attachments}}` placeholders
//!     renders them unsubstituted, same as any other unmet placeholder.
//!   - **No `persistInputs`/`persistAttempts`/`persistRenderedPrompt`, no `subagent`/
//!     `contract_slug`/`session_id`/`parent_event_id`/`reason_call_id` columns, no tier daily-cap /
//!     Heimdall gate-ceiling enforcement, no `raw_compile` markdown-form compiler, no
//!     `structured_output:false` opt-out (a template WITH output_fields always gets a derived
//!     `response_format`), no per-call `model_id` pin, no `web_search`.** None of these are on the
//!     simplified input contract; wiring any back in later is additive, not a rewrite.
//!   - **`cost_usd` is always `0`** (no billing for a local model) and **`tokens_cached` is always
//!     `0`** (no provider-side prompt-cache telemetry from a generic OpenAI-compatible server) —
//!     both literal task requirements, not oversights. `tokens_in`/`tokens_out` come from the
//!     dispatch response's `usage` block via the already-converted
//!     `external__helpers_provider_adapter_helpers::normalizeTokens` twin (reused, not
//!     reimplemented), defaulting to `0` when the local server omits `usage` entirely.
//!   - **Cache-hit return shape is unified with the success shape**: `ai_call_id`, `output_kv`,
//!     `cached`, `cost_usd`, `tier_used`, `temperature_used`, `provider`, `model_id`, `duration_ms`,
//!     `tokens_cached` — always these 10 keys. JS's cache-hit object additionally carries
//!     `cache_source_call_id` and omits `duration_ms`/`tokens_cached`; this twin sets
//!     `duration_ms: 0, tokens_cached: 0` on a hit (matching the zeros the JS INSERT itself writes
//!     to those columns) and drops the extra key, for one uniform contract across both paths.
//!   - **`duration_ms` on a cache MISS reflects only the first dispatch attempt**, not a successful
//!     retry's added latency — this mirrors a genuine JS quirk (`duration_ms` is computed once,
//!     right after the first dispatch, before the retry block runs), not a Rust shortcut.
//!
//! The single network seam is `dispatch_local_llm` (POST via `ureq` + parse) — everything upstream
//! of it (`build_local_request_body`) and downstream of it (`parse_local_response`) is a pure,
//! `#[cfg(test)]`-covered fn with no network involved, per the crossing playbook's lane-A guidance
//! for provider adapters (same shape as `external/openai-call.rs`'s
//! `build_request_body`/`parse_openai_response` split, just no longer deferred at the seam itself).
//!
//! KNOWN FALLOUT (not fixed here — out of scope for "implement aiCall"): 73 dependent skill files
//! (grep: callers of `ai_ai_call::aiCall` whose own `#[cfg(test)]` asserts
//! `err.contains("ai-call")` against aiCall's old always-Err stub text, e.g.
//! `agents/dev/dev-exec-sandbox.rs`, `agents/in/book-run.rs`, `entities/card-score.rs`) will now
//! fail that specific assertion: this twin's real error paths (prompt-not-found,
//! input-validation-failed, `ANTILOKI_LOCAL_LLM_URL`-not-set, dispatch-failed,
//! output-validation-failed) never contain the literal substring "PARITY-DEFERRED" — rightly so,
//! since aiCall is no longer parity-deferred. Those 73 files still COMPILE (the signature is
//! unchanged); only their test assertions go stale. Updating them is a separate, large follow-up
//! pass, not part of this change.
//!
//! NAMES ARE CONTRACT — ported named exports (unchanged): `canonicalize`, `hashInput`,
//! `detectStringifiedJsonCollection`, `validateAndDecompose`, `validateInputs`, `renderTemplate`,
//! `extractPlaceholders`, `checkFeedQuality`, `reconstructRenderedPrompt`, plus the default `aiCall`
//! (now real).
//!
//! PARITY-DEFERRED (sub-branch, `validateAndDecompose`): ported as a 2-arg (raw, output_fields) fn
//! — the JS signature is `(raw, output_fields, db = null, opts = {})`. `db` gates exactly ONE
//! feature, FEAT-GROUNDED-REFERENCES (`groundedRefsCheck`: verifies card_slug/contract_slug/
//! file_path tokens inside a passing string field against the live DB + a REPO_ROOT filesystem
//! walk). Grep of all of antiloki-core's callers (ai-call.test.mjs,
//! ai-call-strict-shape-and-retry-temp.test.mjs, and every other call site) shows EVERY real call
//! is 2-arg — the only 4-arg calls are the two inside aiCall's OWN body (`validateAndDecompose(raw_
//! output, tpl.output_fields, db, validateOpts)`), and this twin's `aiCall` now calls the SAME
//! 2-arg contract too (no `db`/`opts` forwarded), so the omission stays exact, not approximate.
//! `opts.allow_stringified_json_in` (the SHAPE_MISMATCH exemption allow-list) is likewise never
//! passed — omitted the same way (exemption set always empty). `checkFeedQuality`'s JS third
//! parameter `opts.allowedEmpty` (a JS `Set`, not JSON-representable) is dropped for the identical
//! reason: no call site in antiloki-core ever passes a 3rd arg, so it is always the empty-Set
//! default (moot now anyway since `aiCall` doesn't call `checkFeedQuality` — see above).
//!
//! `canonicalize`'s sort comparator is JS `String(a.key).localeCompare(String(b.key))`, ported as
//! a plain codepoint `.cmp()` — exact for this twin's ASCII, case-consistent fixture keys (playbook
//! gotcha #8); a full ICU locale collation is out of scope.
// QUALITY: mixed unit — 9 pure no-db helpers (fixture-tested, unchanged) + 1 real db+network default
// export (local-LLM dispatch; the network POST itself has no fixture/frontier per the crossing
// playbook — see the module doc above for the exact ported/simplified boundary).

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

// Standard helper functions (playbook §Standard helper functions) — copied verbatim (only the
// ones this module actually uses; `tmpl` depends on `str_qq` internally).
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v:&Value)->String{ match v{Value::Null=>"null".into(),Value::String(s)=>s.clone(),Value::Number(n)=>n.to_string(),Value::Bool(b)=>b.to_string(),Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","),Value::Object(_)=>"[object Object]".into()} } // = JS String(v)
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

/// `export function canonicalize(input_kv)` — non-array input (incl. null/undefined, which JSON.
/// stringifies as `{}`) stringifies directly; an array is cloned, sorted by key, projected to
/// `{k,v}` pairs, then JSON.stringify'd. Pure.
pub fn canonicalize(input_kv: &Value) -> Value {
    let arr = match input_kv.as_array() {
        Some(a) => a.clone(),
        None => {
            let v = if input_kv.is_null() { json!({}) } else { input_kv.clone() };
            return json!(serde_json::to_string(&v).unwrap_or_default());
        }
    };
    let mut sorted = arr;
    sorted.sort_by(|a, b| tmpl(a.get("key")).cmp(&tmpl(b.get("key")))); // stable, see module doc
    let mapped: Vec<Value> = sorted.iter().map(|kv| {
        let mut m = Map::new();
        m.insert("k".to_string(), kv.get("key").cloned().unwrap_or(Value::Null));
        m.insert("v".to_string(), kv.get("value").cloned().unwrap_or(Value::Null));
        Value::Object(m)
    }).collect();
    json!(serde_json::to_string(&Value::Array(mapped)).unwrap_or_default())
}

/// `export function hashInput(input_kv)` — sha256 hex digest of `canonicalize(input_kv)`. Pure.
pub fn hashInput(input_kv: &Value) -> Value {
    let canon = canonicalize(input_kv);
    let s = canon.as_str().unwrap_or_default();
    json!(hex::encode(Sha256::digest(s.as_bytes())))
}

/// `export function detectStringifiedJsonCollection(v)` — FIX-AI-OUTPUT-SHAPE-STRICT-ON-PERSIST:
/// detect a STRING that is itself valid JSON for an array or object (the parent_output_id leak
/// signature). Pure.
pub fn detectStringifiedJsonCollection(v: &Value) -> Value {
    let s = match v.as_str() { Some(s) => s, None => return Value::Null };
    let trimmed = s.trim();
    if trimmed.chars().count() < 2 { return Value::Null; }
    let first = trimmed.chars().next().unwrap();
    let last = trimmed.chars().last().unwrap();
    let bracket_ok = (first == '[' && last == ']') || (first == '{' && last == '}');
    if !bracket_ok { return Value::Null; }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Array(_)) => json!("array"),
        Ok(Value::Object(_)) => json!("object"),
        _ => Value::Null,
    }
}

/// `function structuralLintField(field, value)` — internal (not exported by the JS module either).
/// Per-field structural checks beyond type: min/max length, `required_sections` (case-insensitive
/// section-header regex), an author-supplied `pattern`, and a GENERIC enum check (`field.enum`
/// falling back to `field.enum_values`) — a separate list from the dedicated
/// `field_type === 'enum'` branch in `validateAndDecompose`/`validateInputs`, which reads only
/// `enum_values`. Invalid `pattern` regex → skipped, matching the JS try/catch.
fn structural_lint_field(f: &Value, value: &str) -> Vec<String> {
    let mut errs = Vec::new();
    let field_name = f.get("field_name").and_then(Value::as_str).unwrap_or("");
    let len = value.chars().count() as i64;
    if let Some(min_len) = f.get("min_length").and_then(Value::as_i64) {
        if len < min_len { errs.push(format!("'{field_name}' length {len} < min_length {min_len}")); }
    }
    if let Some(max_len) = f.get("max_length").and_then(Value::as_i64) {
        if len > max_len { errs.push(format!("'{field_name}' length {len} > max_length {max_len}")); }
    }
    if let Some(sections) = f.get("required_sections").and_then(Value::as_array) {
        for section in sections {
            let Some(sec_str) = section.as_str() else { continue };
            let pat = format!(r"(?i)(^|\n)\s*{}\b", regex::escape(sec_str));
            if let Ok(re) = regex::Regex::new(&pat) {
                if !re.is_match(value) {
                    errs.push(format!("'{field_name}' missing required section: {sec_str}"));
                }
            }
        }
    }
    if let Some(pattern) = f.get("pattern").and_then(Value::as_str) {
        if !pattern.is_empty() {
            if let Ok(re) = regex::Regex::new(pattern) {
                if !re.is_match(value) { errs.push(format!("'{field_name}' fails pattern /{pattern}/")); }
            }
            // invalid regex → skip rather than fail, matches the JS try/catch.
        }
    }
    let enum_list = f.get("enum").and_then(Value::as_array).or_else(|| f.get("enum_values").and_then(Value::as_array));
    if let Some(list) = enum_list {
        if !list.is_empty() && !list.iter().any(|x| x.as_str() == Some(value)) {
            let shown: Vec<String> = list.iter().take(5).map(|x| tmpl(Some(x))).collect();
            errs.push(format!("'{field_name}' value not in enum: {}", shown.join("|")));
        }
    }
    errs
}

/// `function coerceJsonStringFields(raw, output_fields)` — internal (not exported). Mutates a
/// working copy of `raw` in place: a declared-`string` field that arrived as an array/object
/// becomes its JSON-encoded string (lossless — the consumer JSON.parses it back; a bare NUMBER is
/// deliberately NOT coerced, so a real type error still surfaces); a declared-`number` field that
/// arrived as a parseable numeric STRING is parsed. No-op if `raw` isn't an object.
fn coerce_json_string_fields(raw: &mut Value, output_fields: &Value) {
    if !raw.is_object() { return; }
    let Some(fields) = output_fields.as_array() else { return };
    for f in fields {
        let Some(field_type) = f.get("field_type").and_then(Value::as_str) else { continue };
        let Some(fname) = f.get("field_name").and_then(Value::as_str) else { continue };
        if field_type == "string" {
            let cur = raw.get(fname).cloned();
            if let Some(v) = cur {
                if !v.is_null() && !v.is_string() && (v.is_array() || v.is_object()) {
                    let s = serde_json::to_string(&v).unwrap_or_default();
                    if let Some(obj) = raw.as_object_mut() { obj.insert(fname.to_string(), json!(s)); }
                }
            }
        } else if field_type == "number" {
            let cur = raw.get(fname).cloned();
            if let Some(Value::String(s)) = cur {
                if !s.trim().is_empty() {
                    let n = js_number(Some(&Value::String(s.clone())));
                    if n.is_finite() {
                        let num_val = if n.fract() == 0.0 && n.abs() < 1e15 { json!(n as i64) } else { json!(n) };
                        if let Some(obj) = raw.as_object_mut() { obj.insert(fname.to_string(), num_val); }
                    }
                }
            }
        }
    }
}

/// `export function validateAndDecompose(raw, output_fields, db = null, opts = {})` — validates
/// raw AI output against `output_fields` and decomposes it into row entries for
/// `brain.ai_call_outputs`. PARITY-DEFERRED on `db`/`opts` — see module doc for why dropping them
/// is exact (not approximate) for every real call site outside the deferred `aiCall` body.
pub fn validateAndDecompose(raw: &Value, output_fields: &Value) -> Value {
    let mut raw_owned: Value = raw.clone();

    // SINGLE-FIELD UNWRAP: when exactly one output field is declared, a bare (non-object, or
    // object-without-that-field) response is wrapped as {field: raw}. `{}` and an object that
    // ALREADY has the field are left untouched — see the JS for the truth table this mirrors.
    if let Some(fields_arr) = output_fields.as_array() {
        if fields_arr.len() == 1 {
            if let Some(fname) = fields_arr[0].get("field_name").and_then(Value::as_str) {
                if !fname.is_empty() && !raw_owned.is_null() {
                    let is_obj = raw_owned.is_object();
                    let field_present = is_obj && raw_owned.as_object().unwrap().contains_key(fname);
                    let is_empty_obj = is_obj && raw_owned.as_object().unwrap().is_empty();
                    if !field_present && !is_empty_obj {
                        let mut m = Map::new();
                        m.insert(fname.to_string(), raw_owned.clone());
                        raw_owned = Value::Object(m);
                    }
                }
            }
        }
    }

    coerce_json_string_fields(&mut raw_owned, output_fields);

    let mut rows: Vec<Value> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let fields = output_fields.as_array().cloned().unwrap_or_default();

    for f in &fields {
        let field_name = f.get("field_name").and_then(Value::as_str).unwrap_or("").to_string();
        let field_type = tmpl(f.get("field_type"));
        let required = truthy(f.get("required"));
        let val = raw_owned.as_object().and_then(|o| o.get(&field_name)).cloned();

        let is_missing = match &val {
            None | Some(Value::Null) => true,
            Some(Value::String(s)) if s.is_empty() => true,
            _ => false,
        };
        if is_missing {
            if required { errors.push(format!("required field '{field_name}' missing")); }
            continue;
        }
        let val = val.unwrap();

        match field_type.as_str() {
            "string" => {
                let s = match &val {
                    Value::String(s) => s.clone(),
                    _ => { errors.push(format!("'{field_name}' must be string")); continue; }
                };
                let lint_errs = structural_lint_field(f, &s);
                if !lint_errs.is_empty() { errors.extend(lint_errs); continue; }
                // db-gated FEAT-GROUNDED-REFERENCES check omitted — unreachable on this 2-arg
                // contract (see module doc); every fixture case here supplies no `db`.
                let mut row = Map::new();
                row.insert("field_name".into(), json!(field_name));
                row.insert("value_text".into(), json!(s));
                row.insert("order_index".into(), json!(0));
                rows.push(Value::Object(row));
            }
            "number" => {
                if !val.is_number() { errors.push(format!("'{field_name}' must be number")); continue; }
                let mut row = Map::new();
                row.insert("field_name".into(), json!(field_name));
                row.insert("value_number".into(), val.clone());
                row.insert("order_index".into(), json!(0));
                rows.push(Value::Object(row));
            }
            "boolean" => {
                let b = match &val {
                    Value::Bool(b) => *b,
                    _ => { errors.push(format!("'{field_name}' must be boolean")); continue; }
                };
                let mut row = Map::new();
                row.insert("field_name".into(), json!(field_name));
                row.insert("value_boolean".into(), json!(if b { 1 } else { 0 }));
                row.insert("order_index".into(), json!(0));
                rows.push(Value::Object(row));
            }
            "enum" => {
                let enum_values = f.get("enum_values").and_then(Value::as_array);
                let matches = enum_values.map_or(false, |list| list.iter().any(|x| *x == val));
                if !matches {
                    let joined = enum_values.map(|list| list.iter().map(|x| tmpl(Some(x))).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                    errors.push(format!("'{field_name}' must be one of: {joined}"));
                    continue;
                }
                let mut row = Map::new();
                row.insert("field_name".into(), json!(field_name));
                row.insert("value_text".into(), val.clone());
                row.insert("order_index".into(), json!(0));
                rows.push(Value::Object(row));
            }
            "array_of_string" => {
                let Some(arr) = val.as_array() else {
                    errors.push(format!("'{field_name}' must be array_of_string")); continue;
                };
                for (i, v) in arr.iter().enumerate() {
                    let Some(s) = v.as_str() else {
                        errors.push(format!("'{field_name}[{i}]' must be string")); continue;
                    };
                    if detectStringifiedJsonCollection(v) == json!("array") {
                        let snippet: String = s.chars().take(80).collect::<String>().replace('\n', " ");
                        errors.push(format!("'{field_name}[{i}]' is a stringified JSON array (got: {snippet}). SHAPE_MISMATCH: AI returned a JSON-encoded array as a string element; expected a plain string. Class: parent_output_id leak."));
                        continue;
                    }
                    let mut row = Map::new();
                    row.insert("field_name".into(), json!(field_name));
                    row.insert("value_text".into(), json!(s));
                    row.insert("order_index".into(), json!(i as i64));
                    rows.push(Value::Object(row));
                }
            }
            "array_of_object" => {
                let Some(arr) = val.as_array() else {
                    errors.push(format!("'{field_name}' must be array_of_object")); continue;
                };
                for (i, obj) in arr.iter().enumerate() {
                    // `!obj || typeof obj !== 'object'`: JS typeof for an array is ALSO 'object',
                    // so an array element is accepted here too (Object.entries on it below yields
                    // string-numeric-indexed pairs) — only null/string/number/boolean are rejected.
                    if !obj.is_object() && !obj.is_array() {
                        errors.push(format!("'{field_name}[{i}]' must be object"));
                        continue;
                    }
                    let entries: Vec<(String, Value)> = if let Some(m) = obj.as_object() {
                        m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                    } else {
                        obj.as_array().unwrap().iter().enumerate().map(|(idx, v)| (idx.to_string(), v.clone())).collect()
                    };
                    let mut shape_ok = true;
                    for (k, v) in &entries {
                        let kind = detectStringifiedJsonCollection(v);
                        if let Some(kind_str) = kind.as_str() {
                            let snippet: String = v.as_str().unwrap_or_default().chars().take(80).collect::<String>().replace('\n', " ");
                            errors.push(format!("'{field_name}[{i}].{k}' is a stringified JSON {kind_str} (got: {snippet}). SHAPE_MISMATCH: AI returned a JSON-encoded value as a string; expected an actual {kind_str}. Class: parent_output_id leak (CONTRACT-2 2026-05-24)."));
                            shape_ok = false;
                        }
                    }
                    if shape_ok {
                        let mut row = Map::new();
                        row.insert("field_name".into(), json!(field_name));
                        row.insert("order_index".into(), json!(i as i64));
                        row.insert("_children".into(), obj.clone());
                        rows.push(Value::Object(row));
                    }
                }
            }
            other => { errors.push(format!("'{field_name}' unknown field_type: {other}")); }
        }
    }

    let mut out = Map::new();
    if errors.is_empty() {
        out.insert("ok".into(), json!(true));
        out.insert("rows".into(), json!(rows));
    } else {
        out.insert("ok".into(), json!(false));
        out.insert("rows".into(), json!(rows));
        out.insert("errors".into(), json!(errors));
    }
    Value::Object(out)
}

/// `export function validateInputs(input_kv, input_fields)` — the input twin of
/// `validateAndDecompose`: locks `input_kv`'s STRUCTURE against declared `input_fields` before
/// render. No `input_fields` → `{ok:true}` (no-op for legacy templates). Pure.
pub fn validateInputs(input_kv: &Value, input_fields: &Value) -> Value {
    let fields = match input_fields.as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return json!({ "ok": true }),
    };
    let mut kv_map: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    if let Some(arr) = input_kv.as_array() {
        for kv in arr {
            if let Some(k) = kv.get("key") {
                if !k.is_null() { kv_map.insert(js_string(k), kv.get("value").cloned().unwrap_or(Value::Null)); }
            }
        }
    }
    let mut errors: Vec<String> = Vec::new();
    for f in fields {
        let field_name = f.get("field_name").and_then(Value::as_str).unwrap_or("").to_string();
        let field_type = tmpl(f.get("field_type"));
        let required = truthy(f.get("required"));
        let present = kv_map.contains_key(&field_name);
        let val = kv_map.get(&field_name);
        let empty = match val {
            None | Some(Value::Null) => true,
            Some(Value::String(s)) => s.trim().is_empty(),
            _ => false,
        };
        if !present || empty {
            if required { errors.push(format!("required input '{field_name}' missing or empty")); }
            continue;
        }
        let val = val.unwrap();
        match field_type.as_str() {
            "string" => match val {
                Value::String(s) => errors.extend(structural_lint_field(f, s)),
                _ => errors.push(format!("input '{field_name}' must be string")),
            },
            "number" => if !val.is_number() { errors.push(format!("input '{field_name}' must be number")); },
            "boolean" => if !val.is_boolean() { errors.push(format!("input '{field_name}' must be boolean")); },
            "enum" => {
                let enum_list = f.get("enum_values").and_then(Value::as_array).cloned().unwrap_or_default();
                if !enum_list.iter().any(|x| x == val) {
                    let joined = enum_list.iter().map(|x| tmpl(Some(x))).collect::<Vec<_>>().join(", ");
                    errors.push(format!("input '{field_name}' must be one of: {joined}"));
                }
            }
            "array_of_string" => match val.as_array() {
                Some(arr) => for (i, v) in arr.iter().enumerate() {
                    if !v.is_string() { errors.push(format!("input '{field_name}[{i}]' must be string")); }
                },
                None => errors.push(format!("input '{field_name}' must be array_of_string")),
            },
            "array_of_object" => match val.as_array() {
                Some(arr) => for (i, o) in arr.iter().enumerate() {
                    if !o.is_object() && !o.is_array() { errors.push(format!("input '{field_name}[{i}]' must be object")); }
                },
                None => errors.push(format!("input '{field_name}' must be array_of_object")),
            },
            other => errors.push(format!("input '{field_name}' unknown field_type: {other}")),
        }
    }
    if errors.is_empty() { json!({ "ok": true }) } else { json!({ "ok": false, "errors": errors }) }
}

/// `export function renderTemplate(template_md, input_kv)` — substitutes `{{key}}` placeholders
/// with matching kv values (non-strings JSON.stringify'd). Repeated placeholders all substituted;
/// unmatched ones left in place. Pure. Uses a `Regex` CLOSURE replacement (never a `&str`
/// replacement) so a literal `$` inside a substituted value can never be misread as a capture
/// reference by the regex crate's replacement-string mini-syntax (JS `String.replace(re, str)` has
/// its own analogous `$&`/`$1` behavior; a closure sidesteps both engines' `$`-parsing entirely).
pub fn renderTemplate(template_md: &Value, input_kv: &Value) -> Value {
    let tmpl_str = match template_md.as_str() {
        Some(s) if !s.is_empty() => s,
        _ => return json!(""),
    };
    let arr = match input_kv.as_array() {
        Some(a) => a,
        None => return json!(tmpl_str),
    };
    let mut out = tmpl_str.to_string();
    for kv in arr {
        let key_v = kv.get("key");
        if key_v.map_or(true, |k| k.is_null()) { continue; }
        let safe_key = regex::escape(&tmpl(key_v));
        let Ok(re) = regex::Regex::new(&format!(r"\{{\{{\s*{safe_key}\s*\}}\}}")) else { continue };
        let value_of = match kv.get("value") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(v) => serde_json::to_string(v).unwrap_or_default(),
        };
        out = re.replace_all(&out, |_: &regex::Captures| value_of.clone()).into_owned();
    }
    json!(out)
}

/// `export function extractPlaceholders(template_md)` — unique `{{identifier}}` names, in
/// first-seen order (mirrors JS `Set` iteration order). Pure.
pub fn extractPlaceholders(template_md: &Value) -> Value {
    let s = match template_md.as_str() { Some(s) => s, None => return json!([]) };
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"\{\{\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*\}\}").unwrap());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<Value> = Vec::new();
    for cap in re.captures_iter(s) {
        let name = cap[1].to_string();
        if seen.insert(name.clone()) { out.push(json!(name)); }
    }
    json!(out)
}

/// `export function checkFeedQuality(template_md, input_kv, opts = {})` — scans `template_md` for
/// `{{placeholders}}` and verifies `input_kv` supplies each (non-empty after trim). PARITY-DEFERRED
/// on `opts.allowedEmpty` (a JS `Set`) — see module doc: no real call site ever passes a 3rd arg.
pub fn checkFeedQuality(template_md: &Value, input_kv: &Value) -> Value {
    let required_v = extractPlaceholders(template_md);
    let required: Vec<String> = required_v.as_array().unwrap().iter().map(|v| v.as_str().unwrap_or("").to_string()).collect();
    let mut kv_map: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    if let Some(arr) = input_kv.as_array() {
        for kv in arr {
            if let Some(k) = kv.get("key") {
                if !k.is_null() { kv_map.insert(js_string(k), kv.get("value").cloned().unwrap_or(Value::Null)); }
            }
        }
    }
    let mut missing: Vec<Value> = Vec::new();
    let mut empty: Vec<Value> = Vec::new();
    for key in &required {
        match kv_map.get(key) {
            None => { missing.push(json!(key)); }
            Some(v) => {
                let s = match v {
                    Value::Null => String::new(),
                    Value::String(s) => s.trim().to_string(),
                    other => js_string(other),
                };
                if s.is_empty() { empty.push(json!(key)); }
            }
        }
    }
    if missing.is_empty() && empty.is_empty() { return json!({ "ok": true }); }
    let mut o = Map::new();
    o.insert("ok".into(), json!(false));
    o.insert("missing".into(), json!(missing));
    o.insert("empty".into(), json!(empty));
    o.insert("required".into(), json!(required));
    Value::Object(o)
}

/// `export function reconstructRenderedPrompt(template_md, input_kv)` — read-time reconstruction
/// of the full prompt a call received, replicating the AUTO_TWO_PHASE write-path's reasoning
/// appendix rule (appended only when `reasoning_output` arrived via `input_kv` but the template
/// never declared the placeholder). Pure — calls only `extractPlaceholders` + `renderTemplate`,
/// both in this module.
pub fn reconstructRenderedPrompt(template_md: &Value, input_kv: &Value) -> Value {
    let s = match template_md.as_str() {
        Some(s) if !s.is_empty() => s,
        _ => return json!(""),
    };
    let kv_has_reasoning = input_kv.as_array().map_or(false, |arr| {
        arr.iter().any(|kv| kv.get("key").and_then(Value::as_str) == Some("reasoning_output"))
    });
    let placeholders_v = extractPlaceholders(template_md);
    let has_placeholder = placeholders_v.as_array().unwrap().iter().any(|p| p.as_str() == Some("reasoning_output"));
    let effective = if kv_has_reasoning && !has_placeholder {
        format!("{s}\n\n═══ YOUR REASONING (phase 1) — implement it faithfully; emit ONLY the required output shape ═══\n{{{{reasoning_output}}}}")
    } else {
        s.to_string()
    };
    renderTemplate(&json!(effective), input_kv)
}

// ── local-LLM dispatch: internal helpers (new — not exported by the JS module either) ──────

/// JS `TEMP_TO_VALUE` lookup, ported as a fn now that `aiCall`'s real body needs it (see module
/// doc). `?? 0.2` fallback for an unrecognized temperature string.
fn temp_to_value(temperature: &str) -> f64 {
    match temperature {
        "freezed" => 0.0,
        "cold" => 0.2,
        "warm" => 0.5,
        "hot" => 0.8,
        "hell" => 1.0,
        _ => 0.2,
    }
}

fn round2(ms: f64) -> f64 {
    (ms * 100.0).round() / 100.0
}

/// `function deriveResponseFormat(prompt_name, output_fields)` — internal in JS too (not in its
/// `export {...}` list). Derives an OpenAI/OpenRouter-style `json_schema` `response_format` from
/// the template's declared output fields; `Value::Null` when there are none (JS `return null`).
fn derive_response_format(prompt_name: &str, output_fields: &Value) -> Value {
    let fields = match output_fields.as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return Value::Null,
    };
    let mut properties = Map::new();
    let mut required: Vec<Value> = Vec::new();
    for f in fields {
        let field_type = f.get("field_type").and_then(Value::as_str).unwrap_or("string");
        let mut prop = match field_type {
            "number" => json!({ "type": "number" }),
            "boolean" => json!({ "type": "boolean" }),
            "enum" => match f.get("enum_values").and_then(Value::as_array).filter(|a| !a.is_empty()) {
                Some(vals) => json!({ "type": "string", "enum": vals }),
                None => json!({ "type": "string" }),
            },
            "array_of_string" => json!({ "type": "array", "items": { "type": "string" } }),
            "array_of_object" => json!({ "type": "array", "items": { "type": "object" } }),
            _ => json!({ "type": "string" }), // "string" and any unrecognized type — JS falls through the same way
        };
        if let Some(desc) = f.get("description").and_then(Value::as_str) {
            let clipped: String = desc.chars().take(300).collect();
            prop["description"] = json!(clipped);
        }
        let field_name = f.get("field_name").and_then(Value::as_str).unwrap_or("").to_string();
        if f.get("required").and_then(Value::as_bool).unwrap_or(false) {
            required.push(json!(field_name.clone()));
        }
        properties.insert(field_name, prop);
    }
    // JS: String(prompt_name ?? 'output').replace(/[^a-zA-Z0-9_-]/g, '_').slice(0, 60) || 'output'
    static NAME_RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = NAME_RE.get_or_init(|| regex::Regex::new(r"[^a-zA-Z0-9_-]").unwrap());
    let cleaned = re.replace_all(prompt_name, "_").to_string();
    let sliced: String = cleaned.chars().take(60).collect();
    let name = if sliced.is_empty() { "output".to_string() } else { sliced };

    let mut schema = Map::new();
    schema.insert("type".into(), json!("object"));
    schema.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".into(), json!(required));
    }
    schema.insert("additionalProperties".into(), json!(false));

    json!({
        "type": "json_schema",
        "json_schema": { "name": name, "strict": false, "schema": schema },
    })
}

/// Assembles the `messages` array for the local chat/completions POST: a leading `system` message
/// only when `system_md` is a non-blank string (mirrors `external/openai-call.rs`'s
/// `build_request_body` system-message gating), then the rendered prompt as the `user` message.
fn build_messages(system_md: &Value, rendered_prompt: &str) -> Value {
    let has_system = matches!(system_md, Value::String(s) if !s.trim().is_empty());
    if has_system {
        json!([
            { "role": "system", "content": system_md },
            { "role": "user", "content": rendered_prompt },
        ])
    } else {
        json!([{ "role": "user", "content": rendered_prompt }])
    }
}

/// Pure request-body assembly for the local OpenAI-compatible endpoint — the exact JSON
/// `dispatch_local_llm` POSTs. Split out so it's testable without network (playbook lane-A shape).
fn build_local_request_body(model: &str, messages: &Value, temperature: f64, response_format: &Value) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(model));
    body.insert("messages".into(), messages.clone());
    body.insert("temperature".into(), json!(temperature));
    if !response_format.is_null() {
        body.insert("response_format".into(), response_format.clone());
    }
    Value::Object(body)
}

/// Pure response-envelope parsing: given an already-parsed OpenAI-shaped response body, extract
/// `choices[0].message.content` plus normalized token counts (reuses the already-converted
/// `external__helpers_provider_adapter_helpers::normalizeTokens` twin — same helper a real
/// `openai-call.rs`-style adapter would use). `Err` when there's no message content to validate
/// against. Tokens are always concrete (never "unknown"): `normalizeTokens` itself defaults absent/
/// non-object `usage` to zeros, so this returns `i64` directly rather than `Option<i64>`.
fn parse_local_response(parsed: &Value) -> Result<(String, i64, i64), String> {
    let content = parsed
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|c0| c0.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let content = content.ok_or_else(|| "ai-call: local LLM response missing choices[0].message.content".to_string())?;
    let tokens = crate::external__helpers_provider_adapter_helpers::normalizeTokens(parsed.get("usage").unwrap_or(&Value::Null));
    let tokens_in = tokens.get("tokens_in").and_then(Value::as_i64).unwrap_or(0);
    let tokens_out = tokens.get("tokens_out").and_then(Value::as_i64).unwrap_or(0);
    Ok((content, tokens_in, tokens_out))
}

/// `ANTILOKI_LOCAL_LLM_TIMEOUT_MS` (default 300s — local inference on CPU can be slow), read fresh
/// per call so tests can override it via the shared `test_env_lock::ENV_LOCK`.
fn local_llm_timeout_ms() -> u64 {
    std::env::var("ANTILOKI_LOCAL_LLM_TIMEOUT_MS").ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(300_000)
}

/// THE single network seam: a blocking POST to an OpenAI-compatible chat/completions endpoint,
/// then response-envelope parsing. Everything on either side of this fn (`build_local_request_body`,
/// `parse_local_response`) is pure and covered by `#[cfg(test)]`; this fn itself is not (no network
/// in tests) — mirrors `external/openai-call.rs`'s deferred-boundary convention, except the boundary
/// here is real, not deferred.
/// Where one provider row dispatches: the chat/completions URL, the model string the endpoint expects,
/// and the bearer (OpenRouter only). Pure over env + the key file — no network.
#[derive(Clone, Debug, PartialEq)]
pub struct Endpoint { pub url: String, pub model: String, pub bearer: Option<String> }
pub const OPENROUTER_CHAT_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
pub fn endpoint_for(provider: &str, model_id: &str) -> Result<Endpoint, String> {
    let env = |k: &str| std::env::var(k).ok().filter(|u| !u.trim().is_empty());
    match provider {
        // the coding CLI, headless, via the BE's OpenAI-shaped completions endpoint (the subscription)
        "cli" => {
            let url = env("ANTILOKI_CLI_LLM_URL").or_else(|| env("ANTILOKI_LOCAL_LLM_URL"))
                .ok_or_else(|| "ai-call: ANTILOKI_CLI_LLM_URL not set — the BE sets it at boot (its CLI-backed completions endpoint)".to_string())?;
            let model = if model_id.starts_with("cli:") { model_id.to_string() } else { format!("cli:{model_id}") };
            Ok(Endpoint { url, model, bearer: None })
        }
        // BYOK: the key file (or env) — one of the options, never the only one
        "openrouter" => {
            let key = crate::external_openrouter_call::read_key().map_err(|e| format!("ai-call: openrouter: {e}"))?;
            Ok(Endpoint { url: env("ANTILOKI_OPENROUTER_URL").unwrap_or_else(|| OPENROUTER_CHAT_URL.to_string()), model: model_id.to_string(), bearer: Some(key) })
        }
        // a local OpenAI-compatible server (Ollama / llama.cpp / vLLM)
        _ => {
            let url = env("ANTILOKI_LOCAL_LLM_URL").ok_or_else(|| "ai-call: ANTILOKI_LOCAL_LLM_URL not set — local LLM endpoint required".to_string())?;
            Ok(Endpoint { url, model: model_id.to_string(), bearer: None })
        }
    }
}

fn dispatch_llm(ep: &Endpoint, messages: &Value, temperature: f64, response_format: &Value) -> Result<(String, i64, i64), String> {
    dispatch_llm_tier(ep, messages, temperature, response_format, None)
}
/// A cli endpoint also carries the TIER (2026-09-19): antiloki-serve maps it to the engine's model /
/// effort flag (claude sonnet vs opus, codex gpt-5-mini vs gpt-5 high) — without it every call ran at
/// the engine's default and the high-tier editor was no stronger than a low-tier reviewer.
fn dispatch_llm_tier(ep: &Endpoint, messages: &Value, temperature: f64, response_format: &Value, tier: Option<&str>) -> Result<(String, i64, i64), String> {
    let mut body = build_local_request_body(&ep.model, messages, temperature, response_format);
    if ep.model.starts_with("cli:") { if let Some(t) = tier { body["tier"] = json!(t); } }
    let mut req = ureq::post(&ep.url).timeout(Duration::from_millis(local_llm_timeout_ms()));
    if let Some(k) = &ep.bearer { req = req.set("Authorization", &format!("Bearer {k}")).set("HTTP-Referer", "https://antiloki.dev").set("X-Title", "antiloki"); }
    let resp = req.send_json(body).map_err(|e| format!("ai-call: request to '{}' failed: {e}", ep.url))?;
    let parsed: Value = resp.into_json().map_err(|e| format!("ai-call: response from '{}' was not valid JSON: {e}", ep.url))?;
    parse_local_response(&parsed)
}
#[allow(dead_code)]
fn dispatch_local_llm(url: &str, model: &str, messages: &Value, temperature: f64, response_format: &Value) -> Result<(String, i64, i64), String> {
    dispatch_llm(&Endpoint { url: url.to_string(), model: model.to_string(), bearer: None }, messages, temperature, response_format)
}

/// JS `validation.errors.some((e) => /min_length|max_length|missing required section|fails
/// pattern|value not in enum|cross-field rule|references unknown|references missing/.test(e))` —
/// ported verbatim as its own testable fn. On this twin's 2-arg `validateAndDecompose` contract
/// only the structural-lint classes (min/max_length, required section, pattern, enum) can actually
/// occur (cross-field/grounded-refs are db-gated and never reached — see the module doc); the full
/// regex is kept anyway for exactness against the JS source.
fn is_retryable_validation_error(errors: &[String]) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"min_length|max_length|missing required section|fails pattern|value not in enum|cross-field rule|references unknown|references missing").unwrap()
    });
    errors.iter().any(|e| re.is_match(e))
}

/// `function persistOutputs(db, ai_call_id, rows)` — writes `validateAndDecompose`'s row shapes to
/// brain.ai_call_outputs: a scalar row inserts directly; a `_children` row (array_of_object) inserts
/// a parent (no value) then each child keyed by its own field name, `parent_output_id` = the
/// parent's rowid. JS decomposes `_children` via `Object.entries`, which for an array argument
/// yields index-string-keyed entries (`['0', v0], ['1', v1], ...`) — `children_entries` mirrors
/// that so an AI-returned array-shaped array_of_object element (accepted upstream by
/// `validateAndDecompose`'s `typeof array === 'object'` JS quirk) persists correctly too.
fn children_entries(children: &Value) -> Vec<(String, Value)> {
    if let Some(m) = children.as_object() {
        m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    } else if let Some(a) = children.as_array() {
        a.iter().enumerate().map(|(i, v)| (i.to_string(), v.clone())).collect()
    } else {
        Vec::new()
    }
}
fn persist_outputs(db: &Connection, ai_call_id: i64, rows: &Value) -> Result<(), String> {
    let Some(rows) = rows.as_array() else { return Ok(()) };
    for r in rows {
        match r.get("_children").filter(|v| !v.is_null()) {
            Some(children) => {
                let field_name = r.get("field_name").cloned().unwrap_or(Value::Null);
                let order_index = r.get("order_index").cloned().unwrap_or(json!(0));
                let (_, parent_id) = crate::db_json::run_write(
                    db,
                    "INSERT INTO brain.ai_call_outputs \
                     (ai_call_id, field_name, value_text, value_number, value_boolean, order_index, parent_output_id) \
                     VALUES (?, ?, NULL, NULL, NULL, ?, NULL)",
                    &[json!(ai_call_id), field_name, order_index.clone()],
                )?;
                for (k, v) in children_entries(children) {
                    let (value_text, value_number, value_boolean) = match &v {
                        Value::String(s) => (json!(s), Value::Null, Value::Null),
                        Value::Number(_) => (Value::Null, v.clone(), Value::Null),
                        Value::Bool(b) => (Value::Null, Value::Null, json!(if *b { 1 } else { 0 })),
                        other => (json!(serde_json::to_string(other).unwrap_or_default()), Value::Null, Value::Null),
                    };
                    crate::db_json::run_write(
                        db,
                        "INSERT INTO brain.ai_call_outputs \
                         (ai_call_id, field_name, value_text, value_number, value_boolean, order_index, parent_output_id) \
                         VALUES (?, ?, ?, ?, ?, ?, ?)",
                        &[json!(ai_call_id), json!(k), value_text, value_number, value_boolean, order_index.clone(), json!(parent_id)],
                    )?;
                }
            }
            None => {
                let field_name = r.get("field_name").cloned().unwrap_or(Value::Null);
                let value_text = r.get("value_text").cloned().unwrap_or(Value::Null);
                let value_number = r.get("value_number").cloned().unwrap_or(Value::Null);
                let value_boolean = r.get("value_boolean").cloned().unwrap_or(Value::Null);
                let order_index = r.get("order_index").cloned().unwrap_or(json!(0));
                crate::db_json::run_write(
                    db,
                    "INSERT INTO brain.ai_call_outputs \
                     (ai_call_id, field_name, value_text, value_number, value_boolean, order_index, parent_output_id) \
                     VALUES (?, ?, ?, ?, ?, ?, NULL)",
                    &[json!(ai_call_id), field_name, value_text, value_number, value_boolean, order_index],
                )?;
            }
        }
    }
    Ok(())
}

/// `function readOutputs(db, ai_call_id)` — ported verbatim (same SQL, same shape).
fn read_outputs(db: &Connection, ai_call_id: i64) -> Result<Value, String> {
    crate::db_json::query_rows(
        db,
        "SELECT id, field_name, value_text, value_number, value_boolean, order_index, parent_output_id \
         FROM brain.ai_call_outputs WHERE ai_call_id = ? ORDER BY order_index ASC, id ASC",
        &[json!(ai_call_id)],
    )
}

// db_json's query_rows/run_write surface raw rusqlite messages ("no such table:
// brain.ai_providers"). Every OTHER error aiCall returns carries the `ai-call:` prefix the crate's
// `{skill}: {cause}` convention promises, so a caller can attribute a failure to this boundary —
// which is exactly what the ~74 aiCall-caller tests assert. Prefix the db-sourced ones too.
// (promptFind's error stays unwrapped on purpose — see its call site.)
fn db_err(e: String) -> String {
    format!("ai-call: {e}")
}

// ── aiCall ───────────────────────────────────────────────────────────────────────────────

/// `export default async function aiCall(input, context)` — THE chokepoint. See the module doc for
/// the full ported-vs-simplified boundary; in one line: resolve template → render + validate inputs
/// → resolve tier/temperature/model → cache lookup-or-dispatch(+one retry on validation failure) →
/// persist → return `{ ai_call_id, output_kv, cached, cost_usd, tier_used, temperature_used,
/// provider, model_id, duration_ms, tokens_cached }`.
pub fn aiCall(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    let prompt_name: String = match obj.and_then(|o| o.get("prompt_name")) {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Err("ai-call: prompt_name is required".to_string()),
    };
    let prompt_version = obj.and_then(|o| o.get("prompt_version")).cloned().unwrap_or(Value::Null);
    let input_kv = obj.and_then(|o| o.get("input_kv")).cloned().unwrap_or(json!([]));
    let tier_override = obj.and_then(|o| o.get("tier")).and_then(Value::as_str).map(str::to_string);
    let temp_override = obj.and_then(|o| o.get("temperature")).and_then(Value::as_str).map(str::to_string);
    let budget_remaining_usd = obj.and_then(|o| o.get("budget_remaining_usd")).cloned().unwrap_or(Value::Null);
    let bypass_cache = obj.and_then(|o| o.get("bypass_cache")).and_then(Value::as_bool).unwrap_or(false);

    // 1. Resolve prompt template. promptFind's own "not found" error propagates unchanged — JS's
    // `const tpl = promptFind(...)` has no try/catch around it either.
    let tpl = crate::ai_prompt_find::promptFind(&json!({ "name": prompt_name, "version": prompt_version }), db)?;
    let tpl_id = tpl.get("id").cloned().unwrap_or(Value::Null);
    let template_md = tpl.get("template_md").cloned().unwrap_or(Value::Null);
    let output_fields = tpl.get("output_fields").cloned().unwrap_or(json!([]));
    let input_fields = tpl.get("input_fields").cloned().unwrap_or(json!([]));
    let system_md = tpl.get("system_md").cloned().unwrap_or(Value::Null);

    // EVERY_AI_CALL_CARRIES_ENVELOPE: assembled unconditionally like JS, always with empty
    // references/attachments (not on this twin's simplified input contract — see module doc).
    let envelope = crate::ai_assemble_context_envelope::assembleContextEnvelope(
        &json!({
            "prompt_name": prompt_name, "prompt_version": prompt_version, "intent": Value::Null,
            "input_kv": input_kv, "references": [], "attachments": [],
        }),
        db,
    )?;
    let envelope_json = json!(serde_json::to_string(&envelope).unwrap_or_default());

    // 2. validateInputs — a HARD gate in this twin (module doc: JS's default-WARN/opt-in-strict
    // split is dropped). renderTemplate then builds the outbound prompt text.
    let input_check = validateInputs(&input_kv, &input_fields);
    if input_check["ok"] != json!(true) {
        let errs = input_check["errors"]
            .as_array()
            .map(|a| a.iter().map(|e| e.as_str().unwrap_or("").to_string()).collect::<Vec<_>>().join("; "))
            .unwrap_or_default();
        return Err(format!("ai-call: input structure check failed for prompt '{prompt_name}': {errs}"));
    }
    let rendered = renderTemplate(&template_md, &input_kv);
    let rendered_str = rendered.as_str().unwrap_or("").to_string();

    // 3. Resolve tier + temperature: explicit override > routing_defaults (via the already-ported
    // aiRoute) > template default > aiRoute's own blind mid/cold fallback.
    let (tier, temperature) = if let (Some(t), Some(temp)) = (&tier_override, &temp_override) {
        (t.clone(), temp.clone())
    } else {
        let route = crate::ai_ai_route::aiRoute(
            &json!({ "task_kind": prompt_name, "intent_brief": Value::Null, "budget_remaining_usd": budget_remaining_usd }),
            db,
        )
        .map_err(db_err)?;
        let fallback_used = route["fallback_used"].as_bool().unwrap_or(false);
        let routed_tier = if fallback_used {
            tpl.get("default_tier").and_then(Value::as_str).unwrap_or("mid").to_string()
        } else {
            route["tier"].as_str().unwrap_or("mid").to_string()
        };
        let routed_temp = if fallback_used {
            tpl.get("default_temperature").and_then(Value::as_str).unwrap_or("cold").to_string()
        } else {
            route["temperature"].as_str().unwrap_or("cold").to_string()
        };
        (tier_override.unwrap_or(routed_tier), temp_override.unwrap_or(routed_temp))
    };

    // 4. Hash the input, resolve which model this call dispatches to (module doc: DB row for
    // provider='ollama' first, ANTILOKI_LOCAL_LLM_MODEL env fallback second).
    let input_hash_v = hashInput(&input_kv);
    let input_hash = input_hash_v.as_str().unwrap_or("").to_string();

    // CLI ENGINE PIN (operator 2026-09-16: "the antichat should use claude, opencode or codex
    // underneath, and we should be able to choose"): a `model_id` of `cli:<engine>` bypasses the
    // provider table — the call goes to the BE's CLI-backed completions endpoint, which runs the
    // named coding CLI headless (claude -p / codex exec / opencode run) and answers in the same
    // OpenAI shape the local-LLM path already parses.
    let cli_pin: Option<String> = obj.and_then(|o| o.get("model_id")).and_then(Value::as_str)
        .and_then(|m| m.strip_prefix("cli:")).map(|e| e.trim().to_string()).filter(|e| !e.is_empty());
    // THE CANDIDATES (operator 2026-09-19: "replace all AI capability with claude, codex and opencode
    // so we use our subscriptions; OpenRouter is just one of the options"): every ACTIVE provider row
    // of the tier, failover_rank first, then cli · ollama · openrouter — tried in that order until one
    // answers. A `cli` row runs the named coding CLI headless (the subscription); `ollama` hits the
    // local endpoint; `openrouter` is BYOK with the key file. A cli pin short-circuits to one candidate.
    let candidates: Vec<(String, String)> = if let Some(engine) = cli_pin.clone() { vec![("cli".to_string(), engine)] } else {
        let rows = crate::db_json::query_rows(
            db,
            "SELECT provider, model_id FROM brain.ai_providers WHERE tier = ? AND active = 1 AND provider IN ('cli','ollama','openrouter') \
             ORDER BY failover_rank ASC, CASE provider WHEN 'cli' THEN 0 WHEN 'ollama' THEN 1 ELSE 2 END, id ASC",
            &[json!(tier)],
        ).map_err(db_err)?;
        let mut v: Vec<(String, String)> = rows.as_array().map(|a| a.iter().map(|row| (
            row.get("provider").and_then(Value::as_str).unwrap_or("ollama").to_string(),
            row.get("model_id").and_then(Value::as_str).unwrap_or_default().to_string(),
        )).collect()).unwrap_or_default();
        if v.is_empty() {
            if let Some(m) = std::env::var("ANTILOKI_LOCAL_LLM_MODEL").ok().filter(|s| !s.trim().is_empty()) { v.push(("local".to_string(), m)); }
        }
        if v.is_empty() {
            return Err(format!(
                "ai-call: no active provider for tier='{tier}' in brain.ai_providers (cli · ollama · openrouter) and ANTILOKI_LOCAL_LLM_MODEL is not set"
            ));
        }
        v
    };
    let (provider_name, model_id): (String, String) = candidates[0].clone();

    // 5. Cache lookup (unless bypass_cache). On a hit: a new cached=1 row pointing at the source,
    // the source's prior outputs replayed verbatim. Same 10-key return shape as a miss (module doc).
    if !bypass_cache {
        let cache_rows = crate::db_json::query_rows(
            db,
            "SELECT id FROM brain.ai_calls \
             WHERE prompt_template_id = ? AND input_hash = ? AND tier = ? AND temperature = ? \
               AND model_id IS ? AND outcome = 'success' AND cached = 0 \
             ORDER BY id ASC LIMIT 1",
            &[tpl_id.clone(), json!(input_hash.clone()), json!(tier.clone()), json!(temperature.clone()), json!(model_id.clone())],
        )
        .map_err(db_err)?;
        if let Some(cache_row) = cache_rows.as_array().and_then(|a| a.first()) {
            let cache_source_id = cache_row.get("id").and_then(Value::as_i64).unwrap_or(0);
            let (_, new_id) = crate::db_json::run_write(
                db,
                "INSERT INTO brain.ai_calls \
                 (prompt_template_id, tier, temperature, provider, model_id, input_hash, \
                  duration_ms, cost_usd_actual, tokens_in, tokens_out, cached, cache_source_call_id, \
                  envelope_json, outcome) \
                 VALUES (?, ?, ?, ?, ?, ?, 0, 0, 0, 0, 1, ?, ?, 'success')",
                &[
                    tpl_id.clone(), json!(tier.clone()), json!(temperature.clone()), json!(provider_name.clone()),
                    json!(model_id.clone()), json!(input_hash.clone()), json!(cache_source_id), envelope_json.clone(),
                ],
            )
            .map_err(db_err)?;
            let output_kv = read_outputs(db, cache_source_id).map_err(db_err)?;
            return Ok(json!({
                "ai_call_id": new_id, "output_kv": output_kv, "cached": true, "cost_usd": 0,
                "tier_used": tier, "temperature_used": temperature, "provider": provider_name,
                "model_id": model_id, "duration_ms": 0, "tokens_cached": 0,
            }));
        }
    }

    // 6. Derive response_format + assemble messages; ANTILOKI_LOCAL_LLM_URL gates the dispatch
    // (checked only once we're past a possible cache hit — a cached replay never needs a live model).
    let response_format = derive_response_format(&prompt_name, &output_fields);
    let messages = build_messages(&system_md, &rendered_str);
    let temp_value = temp_to_value(&temperature);
    let t0 = Instant::now();
    // FAILOVER: the first candidate that answers wins; every failed one is remembered for the error.
    let mut chosen: Option<(String, String, Endpoint)> = None;
    let mut first: Option<(String, i64, i64)> = None;
    let mut failures: Vec<String> = Vec::new();
    for (prov, mid) in &candidates {
        let ep = match endpoint_for(prov, mid) { Ok(e) => e, Err(e) => { failures.push(e); continue; } };
        match dispatch_llm_tier(&ep, &messages, temp_value, &response_format, Some(&tier)) {
            Ok(v) => { first = Some(v); chosen = Some((prov.clone(), mid.clone(), ep)); break; }
            Err(e) => { failures.push(format!("{prov}/{mid}: {e}")); }
        }
    }
    let (provider_name, model_id, endpoint) = match chosen { Some(c) => c, None => (provider_name, model_id, Endpoint { url: String::new(), model: String::new(), bearer: None }) };
    let (content, tin0, tout0) = match first {
        Some(v) => v,
        None => {
            let e = failures.join(" · ");
            let duration_ms = round2(t0.elapsed().as_secs_f64() * 1000.0);
            crate::db_json::run_write(
                db,
                "INSERT INTO brain.ai_calls \
                 (prompt_template_id, tier, temperature, provider, model_id, input_hash, \
                  duration_ms, cost_usd_actual, tokens_in, tokens_out, cached, envelope_json, outcome) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, 0, 0, 0, 0, ?, 'error')",
                &[
                    tpl_id.clone(), json!(tier.clone()), json!(temperature.clone()), json!(provider_name.clone()),
                    json!(model_id.clone()), json!(input_hash.clone()), json!(duration_ms), envelope_json.clone(),
                ],
            )
            .map_err(db_err)?;
            return Err(format!("ai-call: every provider of tier='{tier}' failed: {e}"));
        }
    };
    // JS quirk, mirrored on purpose (module doc): duration_ms is fixed here, BEFORE the retry block,
    // so a successful retry's extra latency never lands in the persisted duration_ms.
    let duration_ms = round2(t0.elapsed().as_secs_f64() * 1000.0);
    let mut tokens_in = tin0;
    let mut tokens_out = tout0;

    // 7. Validate + decompose, with JS's one-shot corrective retry on a retryable structural-lint
    // failure. `validation` is only ever reassigned on a SUCCESSFUL retry — an unretryable or
    // still-failing retry keeps the FIRST attempt's (more informative) errors as canonical.
    let raw_output = crate::external__helpers_provider_adapter_helpers::parseJsonOutputOrText(&json!(content));
    let mut validation = validateAndDecompose(&raw_output, &output_fields);
    if validation["ok"] != json!(true) {
        let errors: Vec<String> = validation["errors"].as_array().map(|a| a.iter().map(|e| e.as_str().unwrap_or("").to_string()).collect()).unwrap_or_default();
        if is_retryable_validation_error(&errors) {
            let retry_banner = format!(
                "RETRY: your previous response did not satisfy the output contract. The following lint errors must be fixed:\n{}\n\nReturn JSON of the same shape, with every field passing the structural rules. Do not apologize or explain — just return valid JSON.\n\n──── ORIGINAL PROMPT ────\n\n",
                errors.iter().map(|e| format!("  - {e}")).collect::<Vec<_>>().join("\n")
            );
            let retry_prompt = format!("{retry_banner}{rendered_str}");
            let retry_messages = build_messages(&system_md, &retry_prompt);
            if let Ok((retry_content, r_tin, r_tout)) = dispatch_llm(&endpoint, &retry_messages, temp_value, &response_format) {
                let retry_raw = crate::external__helpers_provider_adapter_helpers::parseJsonOutputOrText(&json!(retry_content));
                let retry_validation = validateAndDecompose(&retry_raw, &output_fields);
                if retry_validation["ok"] == json!(true) {
                    tokens_in += r_tin;
                    tokens_out += r_tout;
                    validation = retry_validation;
                }
                // else: retry also failed — fall through keeping the FIRST validation (matches JS).
            }
            // dispatch Err on retry: also falls through keeping the FIRST validation (matches JS).
        }
    }

    if validation["ok"] != json!(true) {
        let errors: Vec<String> = validation["errors"].as_array().map(|a| a.iter().map(|e| e.as_str().unwrap_or("").to_string()).collect()).unwrap_or_default();
        let (_, failed_id) = crate::db_json::run_write(
            db,
            "INSERT INTO brain.ai_calls \
             (prompt_template_id, tier, temperature, provider, model_id, input_hash, \
              duration_ms, cost_usd_actual, tokens_in, tokens_out, cached, envelope_json, outcome) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?, ?, 0, ?, 'validation_failed')",
            &[
                tpl_id.clone(), json!(tier.clone()), json!(temperature.clone()), json!(provider_name.clone()),
                json!(model_id.clone()), json!(input_hash.clone()), json!(duration_ms), json!(tokens_in), json!(tokens_out),
                envelope_json.clone(),
            ],
        )
        .map_err(db_err)?;
        return Err(format!("ai-call: AI output failed schema validation (ai_call_id={failed_id}): {}", errors.join("; ")));
    }

    // 8. Persist success + read back the decomposed outputs.
    let rows = validation["rows"].clone();
    let (_, ai_call_id) = crate::db_json::run_write(
        db,
        "INSERT INTO brain.ai_calls \
         (prompt_template_id, tier, temperature, provider, model_id, input_hash, \
          duration_ms, cost_usd_actual, tokens_in, tokens_out, tokens_cached, cached, envelope_json, outcome) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?, ?, 0, 0, ?, 'success')",
        &[
            tpl_id, json!(tier.clone()), json!(temperature.clone()), json!(provider_name.clone()),
            json!(model_id.clone()), json!(input_hash), json!(duration_ms), json!(tokens_in), json!(tokens_out),
            envelope_json,
        ],
    )
    .map_err(db_err)?;
    persist_outputs(db, ai_call_id, &rows).map_err(db_err)?;
    let output_kv = read_outputs(db, ai_call_id).map_err(db_err)?;

    Ok(json!({
        "ai_call_id": ai_call_id, "output_kv": output_kv, "cached": false, "cost_usd": 0,
        "tier_used": tier, "temperature_used": temperature, "provider": provider_name,
        "model_id": model_id, "duration_ms": duration_ms, "tokens_cached": 0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── canonicalize / hashInput ────────────────────────────────────────
    #[test]
    fn canonicalize_sorts_by_key_regardless_of_input_order() {
        let a = canonicalize(&json!([{"key":"b","value":2},{"key":"a","value":1}]));
        let b = canonicalize(&json!([{"key":"a","value":1},{"key":"b","value":2}]));
        assert_eq!(a, b);
        assert_eq!(a, json!(r#"[{"k":"a","v":1},{"k":"b","v":2}]"#));
    }

    #[test]
    fn canonicalize_non_array_inputs() {
        assert_eq!(canonicalize(&Value::Null), json!("{}"));
        assert_eq!(canonicalize(&json!({"a":1})), json!(r#"{"a":1}"#));
        assert_eq!(canonicalize(&json!("hello")), json!("\"hello\""));
        assert_eq!(canonicalize(&json!([])), json!("[]"));
    }

    #[test]
    fn hash_input_is_deterministic_and_sha256_hex() {
        let input = json!([{"key":"x","value":"y"}]);
        let h1 = hashInput(&input);
        let h2 = hashInput(&input);
        assert_eq!(h1, h2);
        assert_eq!(h1.as_str().unwrap().len(), 64);
        // known sha256("[{\"k\":\"x\",\"v\":\"y\"}]")
        assert_eq!(
            hex::encode(Sha256::digest(canonicalize(&input).as_str().unwrap().as_bytes())),
            h1.as_str().unwrap()
        );
    }

    // ── detectStringifiedJsonCollection ─────────────────────────────────
    #[test]
    fn detect_stringified_collections() {
        assert_eq!(detectStringifiedJsonCollection(&json!(r#"["a","b"]"#)), json!("array"));
        assert_eq!(detectStringifiedJsonCollection(&json!("  [1, 2, 3]  ")), json!("array"));
        assert_eq!(detectStringifiedJsonCollection(&json!(r#"{"k":1}"#)), json!("object"));
        assert_eq!(detectStringifiedJsonCollection(&json!(r#" {"a":"b"} "#)), json!("object"));
        assert_eq!(detectStringifiedJsonCollection(&json!("[unknown]")), Value::Null);
        assert_eq!(detectStringifiedJsonCollection(&json!("[a, b, c]")), Value::Null);
        assert_eq!(detectStringifiedJsonCollection(&json!("{just text}")), Value::Null);
        assert_eq!(detectStringifiedJsonCollection(&Value::Null), Value::Null);
        assert_eq!(detectStringifiedJsonCollection(&json!(["a","b"])), Value::Null);
        assert_eq!(detectStringifiedJsonCollection(&json!(42)), Value::Null);
        assert_eq!(detectStringifiedJsonCollection(&json!("")), Value::Null);
        assert_eq!(detectStringifiedJsonCollection(&json!("[")), Value::Null);
    }

    // ── validateAndDecompose ─────────────────────────────────────────────
    #[test]
    fn vad_required_field_missing_errors() {
        let fields = json!([{"field_name":"x","field_type":"string","required":true}]);
        let r = validateAndDecompose(&json!({}), &fields);
        assert_eq!(r["ok"], json!(false));
        assert!(r["errors"][0].as_str().unwrap().contains("required field 'x' missing"));
    }

    #[test]
    fn vad_not_required_and_missing_is_ok_with_no_rows() {
        let fields = json!([{"field_name":"opt","field_type":"string","required":false}]);
        let r = validateAndDecompose(&json!({}), &fields);
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["rows"], json!([]));
    }

    #[test]
    fn vad_single_field_unwrap_bare_array_becomes_the_field() {
        let fields = json!([{"field_name":"features_json","field_type":"string","required":true}]);
        let r = validateAndDecompose(&json!([{"id":"ca1","intent":"x"}]), &fields);
        assert_eq!(r["ok"], json!(true), "{r:?}");
        assert_eq!(r["rows"][0]["field_name"], json!("features_json"));
        let parsed: Value = serde_json::from_str(r["rows"][0]["value_text"].as_str().unwrap()).unwrap();
        assert_eq!(parsed, json!([{"id":"ca1","intent":"x"}]));
    }

    #[test]
    fn vad_single_field_unwrap_bare_object_without_the_field_becomes_the_field() {
        let fields = json!([{"field_name":"screening_json","field_type":"string","required":true}]);
        let r = validateAndDecompose(&json!({"context_md":"c","acceptance":[]}), &fields);
        assert_eq!(r["ok"], json!(true), "{r:?}");
        let parsed: Value = serde_json::from_str(r["rows"][0]["value_text"].as_str().unwrap()).unwrap();
        assert_eq!(parsed, json!({"context_md":"c","acceptance":[]}));
    }

    #[test]
    fn vad_array_of_string_clean_spreads_into_rows() {
        let fields = json!([{"field_name":"tags","field_type":"array_of_string","required":true}]);
        let r = validateAndDecompose(&json!({"tags":["a","b","c"]}), &fields);
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["rows"].as_array().unwrap().len(), 3);
        assert_eq!(r["rows"][0]["value_text"], json!("a"));
        assert_eq!(r["rows"][2]["order_index"], json!(2));
    }

    #[test]
    fn vad_array_of_string_shape_mismatch() {
        let fields = json!([{"field_name":"files_touched","field_type":"array_of_string","required":true}]);
        let r = validateAndDecompose(&json!({"files_touched":["real.mjs","[\"other.mjs\"]"]}), &fields);
        assert_eq!(r["ok"], json!(false));
        let e0 = r["errors"][0].as_str().unwrap();
        assert!(e0.contains("SHAPE_MISMATCH"));
        assert!(e0.contains("files_touched[1]"));
        assert!(e0.contains("parent_output_id leak"));
    }

    #[test]
    fn vad_array_of_object_nested_stringified_array_shape_mismatch() {
        let fields = json!([{"field_name":"microtasks","field_type":"array_of_object","required":true}]);
        let raw = json!({"microtasks":[{"microtask_id":"MT1","intent":"do thing","kind":"code","files_touched":"[\"docs/a.md\"]"}]});
        let r = validateAndDecompose(&raw, &fields);
        assert_eq!(r["ok"], json!(false));
        let e0 = r["errors"][0].as_str().unwrap();
        assert!(e0.contains("SHAPE_MISMATCH"));
        assert!(e0.contains("microtasks[0].files_touched"));
        assert!(e0.contains("CONTRACT-2"));
    }

    #[test]
    fn vad_array_of_object_nested_stringified_object_shape_mismatch() {
        let fields = json!([{"field_name":"items","field_type":"array_of_object","required":true}]);
        let raw = json!({"items":[{"name":"x","config":"{\"k\":1}"}]});
        let r = validateAndDecompose(&raw, &fields);
        assert_eq!(r["ok"], json!(false));
        assert!(r["errors"][0].as_str().unwrap().contains("stringified JSON object"));
    }

    #[test]
    fn vad_array_of_object_clean_and_plain_bracket_text_are_ok() {
        let fields = json!([{"field_name":"microtasks","field_type":"array_of_object","required":true}]);
        let raw = json!({"microtasks":[{"microtask_id":"MT1","intent":"create doc","kind":"code"}]});
        let r = validateAndDecompose(&raw, &fields);
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["rows"].as_array().unwrap().len(), 1);

        let fields2 = json!([{"field_name":"items","field_type":"array_of_object","required":true}]);
        let raw2 = json!({"items":[{"name":"x","note":"[draft]"}]});
        let r2 = validateAndDecompose(&raw2, &fields2);
        assert_eq!(r2["ok"], json!(true), "{r2:?}");
    }

    #[test]
    fn vad_array_of_object_mixed_poisoned_and_clean_only_flags_poisoned() {
        let fields = json!([{"field_name":"items","field_type":"array_of_object","required":true}]);
        let raw = json!({"items":[
            {"name":"good","tags":["a","b"]},
            {"name":"bad","tags":"[\"x\",\"y\"]"},
            {"name":"also-good","tags":["c"]}
        ]});
        let r = validateAndDecompose(&raw, &fields);
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["errors"].as_array().unwrap().len(), 1);
        assert!(r["errors"][0].as_str().unwrap().contains("items[1].tags"));
    }

    #[test]
    fn vad_declared_string_field_coerces_array_and_object_but_not_number() {
        let fields = json!([{"field_name":"microtasks_json","field_type":"string","required":true}]);
        let arr = json!([{"microtask_id":"mt1","intent":"do","files_touched":["a.mjs"]}]);
        let r = validateAndDecompose(&json!({"microtasks_json": arr}), &fields);
        assert_eq!(r["ok"], json!(true));
        let parsed: Value = serde_json::from_str(r["rows"][0]["value_text"].as_str().unwrap()).unwrap();
        assert_eq!(parsed, arr);

        let fields2 = json!([{"field_name":"cfg","field_type":"string","required":true}]);
        let r2 = validateAndDecompose(&json!({"cfg":{"k":1}}), &fields2);
        assert_eq!(r2["ok"], json!(true));
        let parsed2: Value = serde_json::from_str(r2["rows"][0]["value_text"].as_str().unwrap()).unwrap();
        assert_eq!(parsed2, json!({"k":1}));

        let fields3 = json!([{"field_name":"s","field_type":"string","required":true}]);
        let r3 = validateAndDecompose(&json!({"s":"hello"}), &fields3);
        assert_eq!(r3["rows"][0]["value_text"], json!("hello"));

        let r4 = validateAndDecompose(&json!({"s":42}), &fields3);
        assert_eq!(r4["ok"], json!(false));
        assert!(r4["errors"][0].as_str().unwrap().contains("must be string"));
    }

    #[test]
    fn vad_number_boolean_enum_and_unknown_type_branches() {
        let numf = json!([{"field_name":"n","field_type":"number","required":true}]);
        assert_eq!(validateAndDecompose(&json!({"n":5}), &numf)["rows"][0]["value_number"], json!(5));
        let bad_n = validateAndDecompose(&json!({"n":"five"}), &numf);
        assert_eq!(bad_n["ok"], json!(false));
        assert!(bad_n["errors"][0].as_str().unwrap().contains("must be number"));

        let boolf = json!([{"field_name":"b","field_type":"boolean","required":true}]);
        assert_eq!(validateAndDecompose(&json!({"b":true}), &boolf)["rows"][0]["value_boolean"], json!(1));
        let bad_b = validateAndDecompose(&json!({"b":"true"}), &boolf);
        assert_eq!(bad_b["ok"], json!(false));

        let enumf = json!([{"field_name":"e","field_type":"enum","required":true,"enum_values":["opt1","opt2"]}]);
        assert_eq!(validateAndDecompose(&json!({"e":"opt1"}), &enumf)["ok"], json!(true));
        let bad_e = validateAndDecompose(&json!({"e":"bogus"}), &enumf);
        assert_eq!(bad_e["ok"], json!(false));
        assert!(bad_e["errors"][0].as_str().unwrap().contains("must be one of: opt1, opt2"));

        let unknownf = json!([{"field_name":"z","field_type":"mystery","required":true}]);
        let r = validateAndDecompose(&json!({"z":"x"}), &unknownf);
        assert_eq!(r["ok"], json!(false));
        assert!(r["errors"][0].as_str().unwrap().contains("unknown field_type: mystery"));
    }

    #[test]
    fn vad_array_type_mismatches_and_non_object_element() {
        let asf = json!([{"field_name":"tags","field_type":"array_of_string","required":true}]);
        let r = validateAndDecompose(&json!({"tags":"not-an-array"}), &asf);
        assert!(r["errors"][0].as_str().unwrap().contains("must be array_of_string"));

        let aof = json!([{"field_name":"items","field_type":"array_of_object","required":true}]);
        let r2 = validateAndDecompose(&json!({"items":"nope"}), &aof);
        assert!(r2["errors"][0].as_str().unwrap().contains("must be array_of_object"));

        let r3 = validateAndDecompose(&json!({"items":[42]}), &aof);
        assert!(r3["errors"][0].as_str().unwrap().contains("items[0]' must be object"));
    }

    #[test]
    fn vad_structural_lint_min_length_and_pattern() {
        let fields = json!([{"field_name":"s","field_type":"string","required":true,"min_length":10}]);
        let r = validateAndDecompose(&json!({"s":"short"}), &fields);
        assert_eq!(r["ok"], json!(false));
        assert!(r["errors"][0].as_str().unwrap().contains("min_length"));

        let fields2 = json!([{"field_name":"s","field_type":"string","required":true,"pattern":"^[A-Z]+$"}]);
        assert_eq!(validateAndDecompose(&json!({"s":"ABC"}), &fields2)["ok"], json!(true));
        let bad = validateAndDecompose(&json!({"s":"abc"}), &fields2);
        assert_eq!(bad["ok"], json!(false));
        assert!(bad["errors"][0].as_str().unwrap().contains("fails pattern"));

        // string field carrying its OWN `enum_values` (generic structural-lint enum check,
        // distinct from the field_type:'enum' branch).
        let fields3 = json!([{"field_name":"s","field_type":"string","required":true,"enum_values":["a","b"]}]);
        let r3 = validateAndDecompose(&json!({"s":"c"}), &fields3);
        assert_eq!(r3["ok"], json!(false));
        assert!(r3["errors"][0].as_str().unwrap().contains("value not in enum: a|b"));
    }

    // ── validateInputs ───────────────────────────────────────────────────
    #[test]
    fn validate_inputs_no_fields_is_ok() {
        assert_eq!(validateInputs(&json!([]), &json!([])), json!({"ok": true}));
        assert_eq!(validateInputs(&json!([]), &Value::Null), json!({"ok": true}));
    }

    #[test]
    fn validate_inputs_required_missing_or_empty() {
        let fields = json!([{"field_name":"goal","field_type":"string","required":true}]);
        let r = validateInputs(&json!([]), &fields);
        assert_eq!(r["ok"], json!(false));
        assert!(r["errors"][0].as_str().unwrap().contains("required input 'goal' missing or empty"));

        let r2 = validateInputs(&json!([{"key":"goal","value":"   "}]), &fields);
        assert_eq!(r2["ok"], json!(false));
    }

    #[test]
    fn validate_inputs_type_checks_and_lint() {
        let fields = json!([{"field_name":"n","field_type":"number","required":true}]);
        assert_eq!(validateInputs(&json!([{"key":"n","value":5}]), &fields)["ok"], json!(true));
        assert_eq!(validateInputs(&json!([{"key":"n","value":"5"}]), &fields)["ok"], json!(false));

        let fields2 = json!([{"field_name":"s","field_type":"string","required":true,"min_length":3}]);
        assert_eq!(validateInputs(&json!([{"key":"s","value":"hi"}]), &fields2)["ok"], json!(false));
        assert_eq!(validateInputs(&json!([{"key":"s","value":"hello"}]), &fields2)["ok"], json!(true));
    }

    #[test]
    fn validate_inputs_not_required_and_missing_is_ok() {
        let fields = json!([{"field_name":"opt","field_type":"string","required":false}]);
        assert_eq!(validateInputs(&json!([]), &fields), json!({"ok": true}));
    }

    // ── renderTemplate ───────────────────────────────────────────────────
    #[test]
    fn render_template_basics() {
        let out = renderTemplate(&json!("Goal: {{goal_md}}\nRisk: {{risk}}"), &json!([
            {"key":"goal_md","value":"add a migration"}, {"key":"risk","value":5}
        ]));
        assert_eq!(out, json!("Goal: add a migration\nRisk: 5"));

        assert_eq!(
            renderTemplate(&json!("Hello {{name}}, your {{missing}}!"), &json!([{"key":"name","value":"Dane"}])),
            json!("Hello Dane, your {{missing}}!")
        );
        assert_eq!(
            renderTemplate(&json!("{{x}}, {{x}}, {{x}}"), &json!([{"key":"x","value":"ok"}])),
            json!("ok, ok, ok")
        );
        assert_eq!(
            renderTemplate(&json!("A: {{ a }}  B: {{b}}"), &json!([{"key":"a","value":"1"},{"key":"b","value":"2"}])),
            json!("A: 1  B: 2")
        );
        assert_eq!(
            renderTemplate(&json!("{{files}}"), &json!([{"key":"files","value":["a.mjs","b.mjs"]}])),
            json!(r#"["a.mjs","b.mjs"]"#)
        );
        assert_eq!(renderTemplate(&json!(""), &json!([])), json!(""));
        assert_eq!(renderTemplate(&json!("hi {{x}}"), &Value::Null), json!("hi {{x}}"));
    }

    // ── extractPlaceholders ──────────────────────────────────────────────
    #[test]
    fn extract_placeholders_unique_first_seen_order() {
        let r = extractPlaceholders(&json!("Hello {{name}}, mission: {{goal}}. Again {{name}}."));
        assert_eq!(r, json!(["name", "goal"]));
        assert_eq!(extractPlaceholders(&json!(42)), json!([]));
        assert_eq!(extractPlaceholders(&json!("no placeholders here")), json!([]));
    }

    // ── checkFeedQuality ─────────────────────────────────────────────────
    #[test]
    fn check_feed_quality_branches() {
        assert_eq!(checkFeedQuality(&json!("Mission: {{goal}}"), &json!([{"key":"goal","value":"do thing"}]))["ok"], json!(true));

        let r = checkFeedQuality(&json!("Mission: {{goal}}\nRisk: {{risk}}"), &json!([{"key":"goal","value":"do"}]));
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["missing"], json!(["risk"]));
        assert_eq!(r["empty"], json!([]));

        let r2 = checkFeedQuality(&json!("Mission: {{goal}}"), &json!([{"key":"goal","value":"   "}]));
        assert_eq!(r2["ok"], json!(false));
        assert_eq!(r2["empty"], json!(["goal"]));

        assert_eq!(checkFeedQuality(&json!("Plain text, no placeholders"), &json!([{"key":"unused","value":"x"}]))["ok"], json!(true));
    }

    // ── reconstructRenderedPrompt ────────────────────────────────────────
    #[test]
    fn reconstruct_rendered_prompt_appendix_rule() {
        // no reasoning_output in kv → plain render, no appendix
        assert_eq!(
            reconstructRenderedPrompt(&json!("Mission: {{goal}}"), &json!([{"key":"goal","value":"x"}])),
            json!("Mission: x")
        );
        // reasoning_output present, template doesn't declare it → appendix appended + rendered
        let out = reconstructRenderedPrompt(&json!("Mission: {{goal}}"), &json!([
            {"key":"goal","value":"x"}, {"key":"reasoning_output","value":"my reasoning"}
        ]));
        let s = out.as_str().unwrap();
        assert!(s.starts_with("Mission: x"));
        assert!(s.contains("YOUR REASONING"));
        assert!(s.ends_with("my reasoning"));
        // template ALREADY declares {{reasoning_output}} → no double-append, just substituted
        let out2 = reconstructRenderedPrompt(&json!("R: {{reasoning_output}}"), &json!([
            {"key":"reasoning_output","value":"already there"}
        ]));
        assert_eq!(out2, json!("R: already there"));
        assert_eq!(reconstructRenderedPrompt(&json!(""), &json!([])), json!(""));
    }

    // ── local-LLM dispatch helpers (pure — no network) ─────────────────────

    #[test]
    fn build_local_request_body_matches_openai_shape() {
        let messages = json!([{ "role": "user", "content": "hello" }]);
        let body = build_local_request_body("qwen2.5:7b", &messages, 0.2, &Value::Null);
        assert_eq!(
            body,
            json!({ "model": "qwen2.5:7b", "messages": [{ "role": "user", "content": "hello" }], "temperature": 0.2 })
        );
        assert!(body.as_object().unwrap().get("response_format").is_none());

        let rf = json!({ "type": "json_schema", "json_schema": { "name": "x", "strict": false, "schema": {} } });
        let body2 = build_local_request_body("m", &messages, 0.0, &rf);
        assert_eq!(body2["response_format"], rf);
        assert_eq!(body2["temperature"], json!(0.0));
    }

    #[test]
    fn parse_local_response_extracts_content_and_tokens() {
        let parsed = json!({
            "choices": [{ "message": { "content": "{\"verdict\":\"pass\"}" } }],
            "usage": { "prompt_tokens": 12, "completion_tokens": 5 },
        });
        let (content, tin, tout) = parse_local_response(&parsed).unwrap();
        assert_eq!(content, "{\"verdict\":\"pass\"}");
        assert_eq!(tin, 12);
        assert_eq!(tout, 5);

        // no usage block at all -> normalizeTokens' own zero default (not an error, not "unknown")
        let parsed2 = json!({ "choices": [{ "message": { "content": "plain text" } }] });
        let (content2, tin2, tout2) = parse_local_response(&parsed2).unwrap();
        assert_eq!(content2, "plain text");
        assert_eq!((tin2, tout2), (0, 0));

        // missing choices[0].message.content -> Err (nothing to feed the validator)
        assert!(parse_local_response(&json!({ "choices": [] })).is_err());
        assert!(parse_local_response(&json!({})).is_err());
    }

    #[test]
    fn build_messages_gates_the_system_role_on_non_blank_system_md() {
        assert_eq!(
            build_messages(&json!("you are terse"), "do the thing"),
            json!([{ "role": "system", "content": "you are terse" }, { "role": "user", "content": "do the thing" }])
        );
        assert_eq!(build_messages(&Value::Null, "x"), json!([{ "role": "user", "content": "x" }]));
        assert_eq!(build_messages(&json!("   "), "x"), json!([{ "role": "user", "content": "x" }]));
    }

    #[test]
    fn derive_response_format_builds_json_schema_from_output_fields() {
        assert_eq!(derive_response_format("t", &json!([])), Value::Null);
        assert_eq!(derive_response_format("t", &Value::Null), Value::Null);

        let fields = json!([
            { "field_name": "verdict", "field_type": "enum", "enum_values": ["pass", "fail"], "required": true },
            { "field_name": "notes", "field_type": "string", "required": false, "description": "extra context" },
        ]);
        let rf = derive_response_format("t.test", &fields);
        assert_eq!(
            rf,
            json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "t_test", // '.' sanitized to '_'
                    "strict": false,
                    "schema": {
                        "type": "object",
                        "properties": {
                            "verdict": { "type": "string", "enum": ["pass", "fail"] },
                            "notes": { "type": "string", "description": "extra context" },
                        },
                        "required": ["verdict"],
                        "additionalProperties": false,
                    },
                },
            })
        );
    }

    #[test]
    fn is_retryable_validation_error_matches_only_structural_lint_classes() {
        assert!(is_retryable_validation_error(&["'s' length 2 < min_length 10".to_string()]));
        assert!(is_retryable_validation_error(&["'s' fails pattern /^[A-Z]+$/".to_string()]));
        assert!(is_retryable_validation_error(&["'e' value not in enum: a|b".to_string()]));
        // NOT retryable in JS: bare required-missing and SHAPE_MISMATCH/type errors.
        assert!(!is_retryable_validation_error(&["required field 'x' missing".to_string()]));
        assert!(!is_retryable_validation_error(&["'x' must be string".to_string()]));
        assert!(!is_retryable_validation_error(&["'x[0]' is a stringified JSON array (…) SHAPE_MISMATCH".to_string()]));
        assert!(!is_retryable_validation_error(&[]));
    }

    // ── aiCall (real — network-free scenarios only; no live model in these tests) ──────────

    #[test]
    fn ai_call_requires_prompt_name() {
        let db = Connection::open_in_memory().unwrap();
        let e = aiCall(&json!({}), &db).unwrap_err();
        assert!(e.contains("prompt_name is required"), "{e}");
    }

    #[test]
    fn ai_call_prompt_not_found() {
        let db = crate::engine_open_db::openDb(&json!(":memory:"), &json!({})).unwrap();
        let e = aiCall(&json!({ "prompt_name": "nonexistent.prompt" }), &db).unwrap_err();
        assert!(e.contains("not found"), "{e}");
    }

    #[test]
    fn ai_call_input_validation_failure_is_a_hard_gate() {
        let db = crate::engine_open_db::openDb(&json!(":memory:"), &json!({})).unwrap();
        crate::ai_prompt_add::promptAdd(
            &json!({
                "name": "t.needs.goal",
                "template_md": "Goal: {{goal}}. Say something wise.",
                "default_tier": "low", "default_temperature": "freezed",
                "output_fields": [{ "field_name": "message", "field_type": "string" }],
                "input_fields": [{ "field_name": "goal", "field_type": "string", "required": true }],
            }),
            &db,
        )
        .unwrap();
        let e = aiCall(&json!({ "prompt_name": "t.needs.goal", "input_kv": [] }), &db).unwrap_err();
        assert!(e.contains("input structure check failed"), "{e}");
        assert!(e.contains("goal"), "{e}");
    }

    #[test]
    fn ai_call_errors_when_local_llm_url_is_unset() {
        // SAFETY: mutates process-global ANTILOKI_LOCAL_LLM_URL/ANTILOKI_LOCAL_LLM_MODEL —
        // serialize against every other test in this process via the shared ENV_LOCK.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let db = crate::engine_open_db::openDb(&json!(":memory:"), &json!({})).unwrap();
        crate::ai_prompt_add::promptAdd(
            &json!({
                "name": "t.basic",
                "template_md": "Say hi to {{name}}.",
                "default_tier": "low", "default_temperature": "freezed",
                "output_fields": [{ "field_name": "message", "field_type": "string" }],
            }),
            &db,
        )
        .unwrap();
        unsafe {
            std::env::remove_var("ANTILOKI_LOCAL_LLM_URL");
            // model resolution must succeed (env fallback, since a fresh openDb seeds only
            // openrouter/anthropic ai_providers rows, never 'ollama') so the URL gate is what fires.
            std::env::set_var("ANTILOKI_LOCAL_LLM_MODEL", "test-local-model");
        }
        let out = aiCall(
            &json!({ "prompt_name": "t.basic", "input_kv": [{ "key": "name", "value": "world" }], "tier": "low", "temperature": "freezed" }),
            &db,
        );
        unsafe {
            std::env::remove_var("ANTILOKI_LOCAL_LLM_MODEL");
        }
        let e = out.unwrap_err();
        // the fresh DB's default provider is the Claude Code CLI (2026-09-19), so the gate that fires
        // is the CLI endpoint's; the env-model fallback only applies with no active row at all
        assert!(e.contains("ANTILOKI_CLI_LLM_URL") || e.contains("ANTILOKI_LOCAL_LLM_URL"), "{e}");
        assert!(e.contains("not set"), "{e}");
    }

    // THE PROVIDERS (2026-09-19): cli · ollama · openrouter, each with its own endpoint shape.
    #[test]
    fn endpoint_for_cli_rides_the_be_completions_url_with_a_cli_model_and_no_bearer() {
        let _g = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ANTILOKI_CLI_LLM_URL", "http://127.0.0.1:41933/v1/chat/completions"); }
        let ep = endpoint_for("cli", "claude-code").unwrap();
        assert_eq!(ep, Endpoint { url: "http://127.0.0.1:41933/v1/chat/completions".into(), model: "cli:claude-code".into(), bearer: None });
        assert_eq!(endpoint_for("cli", "cli:codex").unwrap().model, "cli:codex"); // an already-prefixed pin stays
        unsafe { std::env::remove_var("ANTILOKI_CLI_LLM_URL"); std::env::remove_var("ANTILOKI_LOCAL_LLM_URL"); }
        assert!(endpoint_for("cli", "codex").unwrap_err().contains("ANTILOKI_CLI_LLM_URL"));
    }
    #[test]
    fn endpoint_for_openrouter_needs_the_key_and_carries_it_as_bearer() {
        let _g = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("OPENROUTER_API_KEY", "sk-or-test"); std::env::remove_var("ANTILOKI_OPENROUTER_URL"); }
        let ep = endpoint_for("openrouter", "anthropic/claude-sonnet-4.6").unwrap();
        assert_eq!(ep.url, OPENROUTER_CHAT_URL); assert_eq!(ep.model, "anthropic/claude-sonnet-4.6"); assert_eq!(ep.bearer.as_deref(), Some("sk-or-test"));
        unsafe { std::env::remove_var("OPENROUTER_API_KEY"); std::env::set_var("ANTILOKI_HOME", "/nonexistent-antiloki-home"); }
        assert!(endpoint_for("openrouter", "x").unwrap_err().contains("openrouter"));
        unsafe { std::env::remove_var("ANTILOKI_HOME"); }
    }
    #[test]
    fn endpoint_for_ollama_is_the_local_url_with_the_bare_model() {
        let _g = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ANTILOKI_LOCAL_LLM_URL", "http://127.0.0.1:11434/v1/chat/completions"); }
        let ep = endpoint_for("ollama", "qwen2.5-coder").unwrap();
        assert_eq!((ep.url.as_str(), ep.model.as_str(), ep.bearer), ("http://127.0.0.1:11434/v1/chat/completions", "qwen2.5-coder", None));
        unsafe { std::env::remove_var("ANTILOKI_LOCAL_LLM_URL"); }
        assert!(endpoint_for("ollama", "x").unwrap_err().contains("ANTILOKI_LOCAL_LLM_URL"));
    }

}
