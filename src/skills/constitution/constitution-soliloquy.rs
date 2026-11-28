//! constitution-soliloquy.rs — twin of skills/constitution/constitution-soliloquy.mjs (db lane,
//! cross-db via judgments/laws through the constitution-paper twin, brain for the prompt template,
//! nb for the notebook item). NAMES ARE CONTRACT: default `constitutionSoliloquy`. THE SOLILOQUY:
//! ONE narrator turns a saved paper into a short listening lecture — chapters scripted by one AI
//! call, then voiced chapter-by-chapter through the LOCAL Kokoro TTS and concatenated into one
//! file. PAPER-ONLY (operator 2026-07-20): `resolvePaperSource` (the already-converted
//! `constitution_constitution_paper` twin) is the ONLY legal source. Same TWO-PHASE wizard shape
//! as the JS: `preview:true` drafts the script only (one AI call, nothing voiced/persisted);
//! passing a confirmed `script` back voices THAT script (no second AI call); neither runs both
//! phases in one call.
//!
//! PARITY-DEFERRED (two boundaries, both already-crossed stubs reused verbatim):
//! - the script call dispatches through `crate::ai_ai_call::aiCall`, which ALWAYS `Err`s (see its
//!   own module doc) — so both iterations of the JS `for (let att=1; att<=2; ...)` retry loop take
//!   the same `catch` branch a real dispatch would on every attempt, exactly mirroring the JS
//!   shape (`attempts` count, `dropped:1`, `parse_excerpt` from the caught message).
//! - the voice call dispatches through `crate::system_kokoro_speak::kokoroPcm`, which ALSO ALWAYS
//!   `Err`s (real Kokoro-82M ONNX inference — see its own module doc) — so the JS
//!   `try { kokoroPcm(...) } catch (e) { return { ok:false, error: 'the voice failed at chapter...' } }`
//!   branch is what this twin's kokoro loop takes on its very first chapter, which is itself a
//!   real, deterministic, testable outcome (not a fabricated one): a script that WOULD have voiced
//!   correctly reliably reports "the voice failed at chapter 1/N: <deferred message>" the same way
//!   a genuinely-unavailable model would in production.
//!
//! Everything else is ported faithfully and IS this twin's deterministic, fixture-reachable
//! surface: the `NODE_TEST_CONTEXT` voice pre-flight guard (JS drops the `context?.tts_fn` half of
//! that check — a JS closure injected via `context` cannot cross the JSON `&Value` boundary this
//! crate uses, so this twin always takes the "no injected fn" arm, the only one any real,
//! non-test caller ever takes), `resolvePaperSource`'s ok/error short-circuit, `label`/`notebook_id`
//! /`swarm_id` derivation (`swarm_id` only ever threads into the deferred `runWithSession` ambient
//! dispatch context — dropped the same way constitution-class.rs drops it), `ensure_prompt`
//! (reusing `ai_prompt_find`/`ai_prompt_add`), the given-script phase-2 shortcut (chapter
//! clip/filter, the <2-usable-chapters refusal), the retry-loop script parse/shape (reusing
//! `constitution_constitution_agent_run::extractJson`), the preview-stats computation
//! (words/tts_chars/est_duration_s), and — beyond the kokoro boundary, ported byte-for-byte from
//! the JS but unreachable by this file's own fixture for the same reason constitution-class.rs's
//! post-aiCall deck code is unreachable — the chapter start_s/dur_s stamping, the wav-file
//! assembly (`wav_tools::encodeWav16`/`concatF32`, both already-crossed twins), the transcript
//! join, and the notebook-item insert (`notebooks_notebook_schema`/`notebooks_notebook_store`).
// QUALITY: mixed unit — deterministic guard/retry/preview/voice-dispatch surface (fully ported,
// fixture-reachable through the deferred kokoroPcm boundary) + post-boundary wav/notebook assembly
// (ported by inspection, unreachable by fixture — same shape as constitution-class.rs).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}
// `const clip = (v, n) => (v == null ? null : String(v).slice(0, n));`
fn clip(v: Option<&Value>, n: usize) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(x) => Some(js_string(x).chars().take(n).collect()),
    }
}

const PROMPT: &str = "constitution-soliloquy";
const SYSTEM_LINE1: &str = "You are ONE narrator recording a short engineering lecture from a study. The listener is a smart engineer with NO screen: never reference figures, code blocks, tables or \"as shown\" — spell every idea in spoken words. REORGANIZE for listening — never read the study's prose; simplify and re-explain in walkthrough order: what this is → how it fits together → why it is this way → what to watch out for → the takeaway.";
const SYSTEM_LINE2: &str = "4-8 chapters. Each chapter: a 3-6 word title + 2-5 spoken sentences, at most ~110 words — one idea per chapter, plain words, concrete. TOTAL under 5 minutes spoken; cut anything that does not teach.";
const SYSTEM_LINE3: &str = "Output ONLY valid JSON, exactly: {\"title\":\"the lecture title, plain\",\"chapters\":[{\"title\":\"3-6 words\",\"speech\":\"2-5 spoken sentences\"}]}. Start with \"{\" immediately. (soliloquy contract v1 — one narrator, walkthrough style)";
fn system_md() -> String { [SYSTEM_LINE1, SYSTEM_LINE2, SYSTEM_LINE3].join("\n") }
const TEMPLATE: &str = "THE LECTURE TOPIC: {{label}}\n\nTHE STUDY (your only source — reorganize it for the ear, never invent beyond it):\n{{markdown}}\n\nWrite the narration JSON.";

// the TTS output format is mp3_44100_64 (CBR) in the legacy tts_fn path: byte length maps
// linearly to time. Dropped from this twin — see module doc (tts_fn is an unrepresentable JS
// closure; only the local Kokoro path is ported).
#[allow(dead_code)]
const TTS_BPS: f64 = 64000.0 / 8.0;

// mirrors JS ensurePrompt: try promptFind; if found AND template_md+system_md already match the
// constants, no-op; otherwise (missing OR stale) promptAdd — no re-fetch after (JS doesn't either).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let e = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(ev) = &e {
        let tpl_ok = ev.get("template_md").and_then(Value::as_str) == Some(TEMPLATE);
        let sys_ok = ev.get("system_md").and_then(Value::as_str) == Some(system_md().as_str());
        if tpl_ok && sys_ok {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": system_md(),
            "default_tier": "mid",
            "default_temperature": "warm",
            "phase_flow": "single_only",
            "max_output_tokens": 2200,
            "output_fields": [{ "field_name": "soliloquy_json", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

// `given.chapters.slice(0,8).map(clip title/speech).filter(speech)` — shared by the phase-2
// given-script path and the AI-parsed retry-loop path.
fn shape_chapters(raw: &[Value]) -> Vec<Value> {
    raw.iter()
        .take(8)
        .map(|c| {
            let title = clip(c.get("title"), 90).unwrap_or_else(|| "(untitled)".to_string());
            let speech = clip(c.get("speech"), 1100).unwrap_or_default();
            json!({ "title": title, "speech": speech })
        })
        .filter(|c| c.get("speech").and_then(Value::as_str).map_or(false, |s| !s.is_empty()))
        .collect()
}

pub fn constitutionSoliloquy(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const db = context?.db; if (!db) throw err.required(...)` — unreachable here
    // (`db: &Connection` is a required, non-nullable reference).
    let is_preview = input.get("preview").and_then(Value::as_bool) == Some(true);

    // the voice pre-flight — an unavailable voice must not burn the script call. JS ALSO checks
    // `!context?.tts_fn` (a test-injectable closure that cannot cross the JSON boundary this
    // twin uses — see module doc); every real, non-test caller has no such fn, so this twin
    // always takes that arm and gates purely on NODE_TEST_CONTEXT, matching real dispatch.
    if !is_preview && std::env::var_os("NODE_TEST_CONTEXT").is_some() {
        return Ok(json!({ "ok": false, "error": "test context — the voice model is refused (inject tts_fn)" }));
    }

    // PAPER-ONLY: the only legal source. A not-ok resolution is returned VERBATIM.
    let paper = crate::constitution_constitution_paper::resolvePaperSource(db, input);
    if paper.get("ok").and_then(Value::as_bool) != Some(true) {
        return Ok(paper);
    }
    let markdown = js_string(paper.get("markdown").unwrap_or(&Value::Null));
    let label = clip(input.get("label"), 140)
        .or_else(|| clip(paper.get("title"), 140))
        .unwrap_or_else(|| "the study".to_string());

    // `Number(input?.notebook_id ?? 0) || null` — falsy (0/NaN) collapses to null.
    let nb_raw = nz(input.get("notebook_id")).cloned().unwrap_or(json!(0));
    let nb_num = js_number(Some(&nb_raw));
    let nb_id: Value = if nb_num != 0.0 && !nb_num.is_nan() { json!(nb_num as i64) } else { Value::Null };

    ensure_prompt(db)?;

    // `String(input?.swarm_id ?? paper.swarm_id ?? '').slice(0, 80) || null`. Only ever threads
    // into the deferred `runWithSession` ambient dispatch context (run-session.rs's own module
    // doc: no Rust equivalent, pure ambient — never data reaching aiCall's input/output). Same
    // drop as constitution-class.rs.
    let _swarm_id_unused = {
        let s = js_string(nz(input.get("swarm_id")).or_else(|| nz(paper.get("swarm_id"))).unwrap_or(&Value::Null));
        let t: String = s.chars().take(80).collect();
        if t.is_empty() { Value::Null } else { json!(t) }
    };

    let md_slice: String = markdown.chars().take(22000).collect();

    let mut attempts: i64 = 0;
    let mut cost: f64 = 0.0;
    let mut ai_call_id: Value = Value::Null;
    let mut text = String::new();
    let mut script: Option<Value> = None;
    let mut last_err: Option<String> = None;

    let given = nz(input.get("script")).cloned();
    let given_chapters_array: Option<Vec<Value>> = given
        .as_ref()
        .and_then(|g| g.get("chapters"))
        .and_then(Value::as_array)
        .cloned();

    if let Some(raw_chapters) = given_chapters_array {
        // phase 2 of the wizard: voice the CONFIRMED script — no second AI call.
        let chapters = shape_chapters(&raw_chapters);
        if chapters.len() >= 2 {
            let title = clip(given.as_ref().and_then(|g| g.get("title")), 140).unwrap_or_else(|| label.clone());
            script = Some(json!({ "title": title, "chapters": chapters }));
        }
        let cost_raw = nz(input.get("script_cost_usd")).cloned().unwrap_or(json!(0));
        cost = js_number(Some(&cost_raw));
        ai_call_id = nz(input.get("script_ai_call_id")).cloned().unwrap_or(Value::Null);
        if script.is_none() {
            return Ok(json!({ "ok": false, "error": "the given script has fewer than 2 usable chapters" }));
        }
    } else {
        for att in 1..=2i64 {
            attempts = att;
            let mut kv = vec![
                json!({ "key": "label", "value": label }),
                json!({ "key": "markdown", "value": md_slice }),
            ];
            if att > 1 {
                kv.push(json!({ "key": "_retry", "value": att.to_string() }));
            }
            let ai_input = json!({
                "prompt_name": PROMPT,
                "input_kv": kv,
                "bypass_cache": att > 1,
                "provider_fn": input.get("provider_fn").cloned().unwrap_or(Value::Null),
            });
            // PARITY-DEFERRED: aiCall always Errs (see module doc); this catch branch is what
            // conform-db always exercises, on both attempts.
            match crate::ai_ai_call::aiCall(&ai_input, db) {
                Err(e) => {
                    last_err = Some(e.chars().take(220).collect());
                    continue;
                }
                Ok(res) => {
                    last_err = None;
                    cost += res.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
                    if let Some(id) = res.get("ai_call_id") {
                        ai_call_id = id.clone();
                    }
                    text = res
                        .get("output_kv")
                        .and_then(Value::as_array)
                        .and_then(|arr| {
                            arr.iter().find(|o| {
                                o.get("field_name").and_then(Value::as_str) == Some("soliloquy_json")
                                    && matches!(o.get("parent_output_id"), None | Some(Value::Null))
                            })
                        })
                        .and_then(|o| o.get("value_text"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let body = crate::constitution_constitution_agent_run::extractJson(&json!(text), 0);
                    if let Some(body) = &body {
                        let chapters_ok = body.is_object() && body.get("chapters").map_or(false, Value::is_array);
                        if chapters_ok {
                            let raw_chapters: Vec<Value> = body.get("chapters").and_then(Value::as_array).cloned().unwrap_or_default();
                            let chapters = shape_chapters(&raw_chapters);
                            if chapters.len() >= 2 {
                                let title = clip(body.get("title"), 140).unwrap_or_else(|| label.clone());
                                script = Some(json!({ "title": title, "chapters": chapters }));
                                break;
                            }
                        }
                    }
                }
            }
        }
        if script.is_none() {
            return Ok(json!({
                "ok": true, "soliloquy": Value::Null, "item": Value::Null,
                "preview": if is_preview { json!(true) } else { Value::Null },
                "attempts": attempts, "dropped": 1,
                "parse_excerpt": last_err.unwrap_or_else(|| text.chars().take(220).collect()),
                "cost_usd": cost, "ai_call_id": ai_call_id,
            }));
        }
    }

    let script = script.expect("script is Some past both branches' early returns");

    if is_preview {
        // the wizard's preview pane: the script + everything the voicing will cost.
        let chapters = script.get("chapters").and_then(Value::as_array).cloned().unwrap_or_default();
        let words: i64 = chapters.iter().map(|c| {
            c.get("speech").and_then(Value::as_str).unwrap_or("").split_whitespace().count() as i64
        }).sum();
        let chars: i64 = chapters.iter().map(|c| {
            c.get("speech").and_then(Value::as_str).unwrap_or("").chars().count() as i64
        }).sum();
        return Ok(json!({
            "ok": true, "preview": true, "script": script, "attempts": attempts, "dropped": 0,
            "cost_usd": cost, "ai_call_id": ai_call_id,
            "stats": {
                "chapters": chapters.len(),
                "words": words,
                "tts_chars": chars,
                "est_duration_s": ((words as f64) / 2.6).round() as i64,
                "paper_slug": paper.get("slug").cloned().unwrap_or(Value::Null),
            },
        }));
    }

    // the voice, chapter by chapter — the legacy injected-tts_fn base64 path is dropped (see
    // module doc); KOKORO presses locally: exact durations from the PCM itself, one wav on
    // disk, $0. PARITY-DEFERRED: kokoroPcm always Errs, so this loop returns at chapter 1/N on
    // every real dispatch — a real, deterministic outcome, not a fabricated success.
    let script_chapters: Vec<Value> = script.get("chapters").and_then(Value::as_array).cloned().unwrap_or_default();
    let voice = nz(input.get("voice_id")).cloned().unwrap_or(json!("af_heart"));
    let mut parts: Vec<Vec<f32>> = Vec::new();
    let mut durs: Vec<f64> = Vec::new();
    for (i, ch) in script_chapters.iter().enumerate() {
        let speech = ch.get("speech").and_then(Value::as_str).unwrap_or("").to_string();
        let kokoro_input = json!({ "text": speech, "voice": voice.clone() });
        match crate::system_kokoro_speak::kokoroPcm(&kokoro_input, &Value::Null) {
            Err(e) => {
                let msg: String = e.chars().take(160).collect();
                return Ok(json!({
                    "ok": false, "cost_usd": cost, "ai_call_id": ai_call_id,
                    "error": format!("the voice failed at chapter {}/{}: {}", i + 1, script_chapters.len(), msg),
                }));
            }
            Ok((samples, _rate)) => {
                // JS: `rate === KOKORO_RATE ? samples : samples` — a dead ternary (both arms are
                // `samples`); duration divides by the KOKORO_RATE CONSTANT, not the returned rate.
                durs.push(samples.len() as f64 / crate::system_kokoro_speak::RATE as f64);
                parts.push(samples);
            }
        }
    }

    // unreachable past this point by this twin's own fixture (kokoroPcm never resolves Ok — see
    // module doc); ported byte-for-byte from the JS, correctness by inspection (constitution-
    // class.rs precedent for its own post-aiCall deck/notebook code).
    let mut at = 0.0f64;
    let mut chapters_out: Vec<Value> = Vec::new();
    for (i, ch) in script_chapters.iter().enumerate() {
        let start_s = (at * 10.0).round() / 10.0;
        let dur_s = (durs[i] * 10.0).round() / 10.0;
        at += durs[i];
        let mut obj: Map<String, Value> = ch.as_object().cloned().unwrap_or_default();
        obj.insert("start_s".into(), json!(start_s));
        obj.insert("dur_s".into(), json!(dur_s));
        chapters_out.push(Value::Object(obj));
    }
    let duration_s = (at * 10.0).round() / 10.0;
    let file_bytes = crate::wav_tools::encodeWav16(&crate::wav_tools::concatF32(&parts), crate::system_kokoro_speak::RATE);
    let ext = "wav";
    let mime = "audio/wav";

    let root = crate::engine_roots::artifacts_dir();
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let stamp = to_base36(now_ms);
    let rel_str = format!("audio/soliloquy-{stamp}.{ext}");
    let audio_dir = root.join("audio");
    let _ = std::fs::create_dir_all(&audio_dir);
    let _ = std::fs::write(root.join(&rel_str), &file_bytes);

    let transcript = chapters_out
        .iter()
        .map(|c| format!("## {}\n\n{}", c.get("title").and_then(Value::as_str).unwrap_or(""), c.get("speech").and_then(Value::as_str).unwrap_or("")))
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut item: Value = Value::Null;
    if !matches!(nb_id, Value::Null) {
        let _ = crate::notebooks_notebook_schema::ensureNotebookSchema(db, &json!("nb."));
        let title = script.get("title").cloned().unwrap_or(Value::Null);
        let opts = json!({
            "kind": "artifact",
            "title": format!("🎙 {}", js_string(&title)),
            "body_md": transcript,
            "pinned": 0,
            "meta": {
                "artifact_kind": "soliloquy",
                "file": rel_str,
                "mime": mime,
                "chapters": chapters_out,
                "duration_s": duration_s,
                "cost_usd": cost,
                "ai_call_id": ai_call_id,
                "paper_slug": paper.get("slug").cloned().unwrap_or(Value::Null),
            },
        });
        item = crate::notebooks_notebook_store::insertItem(db, &nb_id, &opts);
    }

    Ok(json!({
        "ok": true, "item": item, "attempts": attempts, "dropped": 0,
        "cost_usd": cost, "ai_call_id": ai_call_id,
        "soliloquy": { "title": script.get("title").cloned().unwrap_or(Value::Null), "chapters": chapters_out, "file": rel_str, "duration_s": duration_s },
    }))
}

// `Date.now().toString(36)` — base-36 digits, lowercase, no leading zeros (0 itself renders "0").
fn to_base36(mut n: u128) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 { return "0".to_string(); }
    let mut buf = Vec::new();
    while n > 0 {
        buf.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    buf.reverse();
    String::from_utf8(buf).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRAIN_SCHEMA: &str = "
        CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
        CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
        CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
    ";

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS brain; ATTACH ':memory:' AS nb;").unwrap();
        c.execute_batch(BRAIN_SCHEMA).unwrap();
        c
    }

    fn a_paper(c: &Connection) -> i64 {
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(c);
        c.execute("INSERT INTO judgments.review_papers (slug, title, paper_json, markdown_md) VALUES ('paper-one', 'Paper One', '{}', '# the study')", []).unwrap();
        c.last_insert_rowid()
    }

    // env-var test guard: NODE_TEST_CONTEXT is a process-global — serialize with the shared lock
    // (project convention: local per-file mutexes don't prevent cross-file races on the same var).
    fn with_node_test_context<T>(set: bool, f: impl FnOnce() -> T) -> T {
        let _g = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let prior = std::env::var("NODE_TEST_CONTEXT").ok();
        if set { std::env::set_var("NODE_TEST_CONTEXT", "1"); } else { std::env::remove_var("NODE_TEST_CONTEXT"); }
        let r = f();
        match prior { Some(v) => std::env::set_var("NODE_TEST_CONTEXT", v), None => std::env::remove_var("NODE_TEST_CONTEXT") }
        r
    }

    #[test]
    fn no_paper_source_returns_the_resolution_error_verbatim() {
        let c = conn();
        let r = with_node_test_context(false, || constitutionSoliloquy(&json!({}), &c).unwrap());
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("a saved paper is required"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'constitution-soliloquy'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn unknown_paper_id_returns_not_found_verbatim() {
        let c = conn();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        let r = with_node_test_context(false, || constitutionSoliloquy(&json!({ "paper_id": 999 }), &c).unwrap());
        assert_eq!(r, json!({ "ok": false, "error": "no such paper (999)" }));
    }

    #[test]
    fn test_context_guard_refuses_before_paper_resolution_unless_preview() {
        let c = conn();
        // no paper given at all — if the guard fires FIRST (as JS does), no paper-resolution
        // error ever surfaces; if preview is skipped by the guard too, preview still bypasses it.
        let r = with_node_test_context(true, || constitutionSoliloquy(&json!({}), &c).unwrap());
        assert_eq!(r, json!({ "ok": false, "error": "test context — the voice model is refused (inject tts_fn)" }));
    }

    #[test]
    fn preview_bypasses_the_test_context_guard_and_reaches_paper_resolution() {
        let c = conn();
        let r = with_node_test_context(true, || constitutionSoliloquy(&json!({ "preview": true }), &c).unwrap());
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("a saved paper is required"));
    }

    #[test]
    fn saved_paper_reaches_the_deferred_ai_call_and_drops_the_script() {
        let c = conn();
        let pid = a_paper(&c);
        let r = with_node_test_context(false, || constitutionSoliloquy(&json!({ "paper_id": pid }), &c).unwrap());
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["soliloquy"], Value::Null);
        assert_eq!(r["item"], Value::Null);
        assert_eq!(r["attempts"], json!(2));
        assert_eq!(r["dropped"], json!(1));
        assert_eq!(r["cost_usd"], json!(0.0));
        assert_eq!(r["ai_call_id"], Value::Null);
        assert!(r["parse_excerpt"].as_str().unwrap().contains("ai-call"), "{}", r["parse_excerpt"]);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'constitution-soliloquy'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_prompt_writes_then_is_idempotent() {
        let c = conn();
        ensure_prompt(&c).unwrap();
        let found = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), &c).unwrap();
        assert_eq!(found.get("template_md").and_then(Value::as_str), Some(TEMPLATE));
        assert_eq!(found.get("system_md").and_then(Value::as_str), Some(system_md().as_str()));
        ensure_prompt(&c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?", [PROMPT], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn given_script_with_fewer_than_two_usable_chapters_is_refused() {
        let c = conn();
        let pid = a_paper(&c);
        let r = with_node_test_context(false, || constitutionSoliloquy(&json!({
            "paper_id": pid,
            "script": { "title": "T", "chapters": [{ "title": "One", "speech": "only one chapter" }] },
        }), &c).unwrap());
        assert_eq!(r, json!({ "ok": false, "error": "the given script has fewer than 2 usable chapters" }));
    }

    #[test]
    fn given_script_with_two_chapters_wraps_a_voice_failure_with_its_chapter() {
        // What this proves is the ERROR WRAPPING — "the voice failed at chapter N/M" — so the voice
        // must fail for a reason the test controls. It used to rely on kokoroPcm being an
        // unconditional stub; synthesis is real now, so an unguarded run here would spawn node and
        // let kokoro-js download ~90MB, then pass or fail depending on the network. Pointing the
        // bridge at a path that cannot exist makes the failure the test's own doing.
        //
        // NO ENV_LOCK HERE: with_node_test_context below already takes it, and std::sync::Mutex is
        // not reentrant — taking it twice in one test deadlocks the whole suite (it did). The env
        // write is safe under that same guard because it happens before the wrapper is entered and
        // is restored after it returns.
        let prev = std::env::var("ANTILOKI_KOKORO_BRIDGE").ok();
        std::env::set_var("ANTILOKI_KOKORO_BRIDGE", "/nonexistent/kokoro-bridge-for-tests.mjs");
        let restore = || match &prev {
            Some(v) => std::env::set_var("ANTILOKI_KOKORO_BRIDGE", v),
            None => std::env::remove_var("ANTILOKI_KOKORO_BRIDGE"),
        };
        let c = conn();
        let pid = a_paper(&c);
        let r = with_node_test_context(false, || constitutionSoliloquy(&json!({
            "paper_id": pid,
            "script": { "title": "The Lecture", "chapters": [
                { "title": "One", "speech": "first chapter speech" },
                { "title": "Two", "speech": "second chapter speech" },
            ] },
            "script_cost_usd": 0.02,
            "script_ai_call_id": 77,
        }), &c).unwrap());
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["cost_usd"], json!(0.02));
        assert_eq!(r["ai_call_id"], json!(77));
        restore();
        assert!(r["error"].as_str().unwrap().starts_with("the voice failed at chapter 1/2:"), "{}", r["error"]);
    }

    #[test]
    fn preview_returns_script_and_stats_without_reaching_kokoro() {
        let c = conn();
        let pid = a_paper(&c);
        let r = with_node_test_context(false, || constitutionSoliloquy(&json!({
            "preview": true,
            "paper_id": pid,
            "script": { "title": "The Lecture", "chapters": [
                { "title": "One", "speech": "five words in this speech" },
                { "title": "Two", "speech": "three more words here" },
            ] },
        }), &c).unwrap());
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["preview"], json!(true));
        assert_eq!(r["stats"]["chapters"], json!(2));
        // verified: node -e word-split of "five words in this speech" (5) + "three more words here" (4) = 9
        assert_eq!(r["stats"]["words"], json!(9));
        assert_eq!(r["stats"]["paper_slug"], json!("paper-one"));
        assert_eq!(r["dropped"], json!(0));
    }

    #[test]
    fn label_falls_back_to_paper_title_then_the_study() {
        let c = conn();
        let pid = a_paper(&c);
        let r = with_node_test_context(false, || constitutionSoliloquy(&json!({ "paper_id": pid, "label": "  " }), &c).unwrap());
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["dropped"], json!(1));
    }

    #[test]
    fn base36_matches_js_date_now_tostring36_shape() {
        assert_eq!(to_base36(0), "0");
        assert_eq!(to_base36(35), "z");
        assert_eq!(to_base36(36), "10");
    }
}
