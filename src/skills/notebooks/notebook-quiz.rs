//! notebook-quiz.rs — twin of skills/notebooks/notebook-quiz.mjs (db lane, cross-db brain.* via
//! reused prompt/ai-call twins). NAMES ARE CONTRACT: default `notebookQuiz`. EPIC-NOTEBOOKS: an
//! interactive multiple-choice quiz for a notebook's scope — the AI returns STRICT JSON
//! (questions + options + the correct index + a why), grounded in the corpus; the FE renders it
//! as a clickable school-style quiz. Stored as a `quiz` item (`meta.quiz`).
//!
//! `context.db` missing → JS throws `err.contextMissing('notebook-quiz', 'db')`. Unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention as every
//! sibling db-lane twin, e.g. notebook-mindmap.rs). `context.provider_fn` (a JS function
//! reference for provider selection/DI) has no Rust equivalent to carry — threaded as `null` into
//! the constructed aiCall input, same as notebook-mindmap.rs's `provider_fn`.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`. The JS wraps
//! `ensurePrompt` + the `aiCall` await + the response-string parsing in one `try { } catch (e) {
//! return { ok:false, error: ... } }` — a failed/absent provider degrades GRACEFULLY, it never
//! throws (same shape as notebook-mindmap.rs). This twin mirrors that exactly: on `aiCall`
//! failure it returns `Ok({ ok:false, error: "notebook-quiz AI failed: <deferred message> — check
//! the OpenRouter provider/model." })`, never propagating the deferred stub's `Err` up to the
//! caller. Everything else — guards, notebook lookup, corpus resolution (reusing the already-
//! converted `notebooks_notebook_corpus::resolveCorpus` twin), `ensurePrompt` (reusing the
//! already-converted `ai_prompt_find`/`ai_prompt_add` twins), and the response-string parsing/
//! `sanitize` question-shaping helper — is the real, fully ported deterministic surface.
//! `sanitize` is ported as its own PURE function and unit-tested directly with canned JSON values
//! — unreachable from `notebookQuiz` itself (the deferred `aiCall` folds to the catch branch
//! before it runs) but correct by construction, same pattern as notebook-mindmap.rs's `sanitize`.
// QUALITY: db-read/write lane (fully ported, fixture-reachable up to the deferred aiCall
// boundary) + 1 pure response-shaping helper (unreachable from the default export, unit-tested
// directly) + 1 bespoke/deferred aiCall dispatch (no fixture/frontier).

use rusqlite::Connection;
use serde_json::{json, Map, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
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
fn falsy_num(n: f64) -> bool { n == 0.0 || n.is_nan() }

const PROMPT: &str = "notebook.quiz.v1";
const SYSTEM: &str = "You write a multiple-choice knowledge quiz about a software scope, grounded ONLY in the provided context. Output STRICT JSON — no prose, no markdown, no code fences.";
const TEMPLATE: &str = "NOTEBOOK SCOPE: {{scope_title}}

=== SOURCES (code + signals) ===
{{code_context}}

Return a QUIZ as STRICT JSON (no markdown / no fences):
{\"questions\":[{\"q\":\"<question>\",\"options\":[\"<a>\",\"<b>\",\"<c>\",\"<d>\"],\"answer\":<0-based index of the correct option>,\"why\":\"<one line explaining the answer>\"}]}
Rules: 6–10 questions; EXACTLY 4 options each; exactly one correct answer (the index); ground every question in the scope. Output ONLY the JSON object.";

fn prompt_output_fields() -> Value {
    json!([{ "field_name": "quiz_json", "field_type": "string", "required": true }])
}

// `function ensurePrompt(db)`: try promptFind, ignore any error → null; if missing, promptAdd.
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let found = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).is_ok();
    if found {
        return Ok(());
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "high",
            "default_temperature": "cold",
            "max_output_tokens": 2200,
            "phase_flow": "single_only",
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    Ok(())
}

// `function sanitize(obj)` — `qs.slice(0, 12)` questions; each needs a non-empty `q`
// (`q.q ?? q.question`) and `options.length >= 2` (`options` mapped to trimmed strings, filtered
// non-empty, capped at 6). `answer` (`q.answer ?? q.correct`) coerced via `Number(...)`; falls
// back to 0 unless it's an integer within `[0, options.length)`. `why` capped at 240 chars.
fn sanitize(obj: &Value) -> Vec<Value> {
    let qs: Vec<Value> = obj.get("questions").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out = Vec::new();
    for q in qs.into_iter().take(12) {
        let question_raw = q.get("q").filter(|v| !v.is_null()).or_else(|| q.get("question").filter(|v| !v.is_null()));
        let question = js_string(question_raw.unwrap_or(&Value::Null)).trim().to_string();
        let options: Vec<Value> = q
            .get("options")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|o| json!(js_string(o).trim().to_string()))
            .filter(|o| truthy(Some(o)))
            .take(6)
            .collect();
        if question.is_empty() || options.len() < 2 {
            continue;
        }
        let answer_raw = q.get("answer").filter(|v| !v.is_null()).or_else(|| q.get("correct").filter(|v| !v.is_null()));
        let answer_n = js_number(answer_raw.or(Some(&json!(0))));
        let answer: i64 = if answer_n.fract() == 0.0 && answer_n.is_finite() && answer_n >= 0.0 && (answer_n as i64) < options.len() as i64 {
            answer_n as i64
        } else {
            0
        };
        let why: String = q.get("why").filter(|v| !v.is_null()).map(js_string).unwrap_or_default().chars().take(240).collect();
        out.push(json!({ "q": question, "options": options, "answer": answer, "why": why }));
    }
    out
}

pub fn notebookQuiz(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const db = context?.db; if (!db) throw err.contextMissing(...)` — unreachable here
    // (Rust's `db: &Connection` is never absent). `context.provider_fn` has no Rust carrier — see
    // module doc; passed as `null` into the constructed aiCall input below.
    let provider_fn = Value::Null;
    crate::notebooks_notebook_schema::ensureNotebookSchema(db, &json!("nb."));

    let id_num = js_number(input.get("id"));
    if falsy_num(id_num) {
        return Err("notebook-quiz: id is required".to_string());
    }
    let id_val = json!(id_num);
    let nb = crate::notebooks_notebook_store::getNotebook(db, &id_val);
    if nb.is_null() {
        return Ok(json!({ "ok": false, "error": format!("notebook {} not found", id_num as i64) }));
    }

    let corpus = crate::notebooks_notebook_corpus::resolveCorpus(&nb, db)?;
    let blocks: Vec<Value> = corpus.get("blocks").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let code_context = if blocks.is_empty() {
        "(no sources — set a scope or add sources.)".to_string()
    } else {
        blocks
            .iter()
            .map(|b| {
                let label = js_string(b.get("label").unwrap_or(&Value::Null)).to_uppercase();
                let text = js_string(b.get("text").unwrap_or(&Value::Null));
                format!("=== {label} ===\n{text}")
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };

    let nb_title = nb.get("title").cloned().unwrap_or(Value::Null);

    // ── the AI wave: PARITY-DEFERRED from here. JS wraps ensurePrompt + the aiCall await + the
    // response-string parsing in one try/catch — a failed/absent provider degrades GRACEFULLY,
    // it never throws. Mirrored here as one closure whose Err short-circuits to the catch branch
    // below (same shape as notebook-mindmap.rs); everything inside is real, ported logic — the
    // success path (`quiz`/`ai_m` populated) is unreachable in THIS build only because
    // `crate::ai_ai_call::aiCall` always returns `Err` (the deferred stub), never because it was
    // skipped in the port.
    let wave: Result<(Value, Value), String> = (|| {
        ensure_prompt(db)?;
        let tier = if truthy(input.get("tier")) { input.get("tier").cloned().unwrap() } else { json!("high") };
        let model_id = input.get("model_id").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null);
        let code_context_capped: String = code_context.chars().take(16000).collect();
        let ai_input = json!({
            "prompt_name": PROMPT,
            "tier": tier,
            "model_id": model_id,
            "provider_fn": provider_fn,
            "input_kv": [
                { "key": "scope_title", "value": nb_title },
                { "key": "code_context", "value": code_context_capped },
            ],
        });
        let res = crate::ai_ai_call::aiCall(&ai_input, db)?;
        let raw = res
            .get("output_kv")
            .and_then(Value::as_array)
            .and_then(|arr| {
                arr.iter().find(|o| {
                    o.get("field_name").and_then(Value::as_str) == Some("quiz_json")
                        && o.get("parent_output_id").map_or(true, |v| v.is_null())
                })
            })
            .and_then(|o| o.get("value_text"))
            .filter(|v| truthy(Some(v)))
            .cloned()
            .unwrap_or(json!(""));
        let ai_m = crate::notebooks_notebook_store::aiMeta(db, &res);

        // `raw.replace(/^```[a-z]*\n?/i, '').replace(/```\s*$/, '').trim()` then locate the
        // outermost `{...}` slice and JSON.parse it, sanitize()-ing on success, `null` on failure.
        let mut s = js_string(&raw);
        if let Some(rest) = s.strip_prefix("```") {
            let lang_end = rest.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(rest.len());
            let (_lang, after_lang) = rest.split_at(lang_end);
            let after_fence = after_lang.strip_prefix('\n').unwrap_or(after_lang);
            s = after_fence.to_string();
        }
        let trimmed_end = s.trim_end().to_string();
        let s = if let Some(idx) = trimmed_end.rfind("```") {
            if trimmed_end[idx + 3..].trim().is_empty() { trimmed_end[..idx].to_string() } else { trimmed_end.clone() }
        } else {
            trimmed_end.clone()
        };
        let s = s.trim().to_string();
        let a = s.find('{');
        let b = s.rfind('}');
        let quiz: Vec<Value> = match (a, b) {
            (Some(a), Some(b)) if b > a => match serde_json::from_str::<Value>(&s[a..=b]) {
                Ok(parsed) => sanitize(&parsed),
                Err(_) => Vec::new(),
            },
            _ => Vec::new(),
        };
        Ok((json!(quiz), ai_m))
    })();

    let (quiz, ai_m) = match wave {
        Ok(v) => v,
        Err(e) => {
            return Ok(json!({ "ok": false, "error": format!("notebook-quiz AI failed: {e} — check the OpenRouter provider/model.") }));
        }
    };
    let quiz_arr = quiz.as_array().cloned().unwrap_or_default();
    if quiz_arr.is_empty() {
        return Ok(json!({ "ok": false, "error": "the model did not return a valid quiz — try again." }));
    }

    let nb_title_s = js_string(nb.get("title").unwrap_or(&Value::Null));
    let body_md = format!("Quiz of **{nb_title_s}** — {} questions.", quiz_arr.len());
    let mut meta: Map<String, Value> = ai_m.as_object().cloned().unwrap_or_default();
    meta.insert("quiz".into(), quiz);
    let item = crate::notebooks_notebook_store::insertItem(
        db,
        &id_val,
        &json!({ "kind": "quiz", "title": format!("Quiz · {} questions", quiz_arr.len()), "body_md": body_md, "meta": Value::Object(meta), "pinned": 0 }),
    );
    Ok(json!({ "ok": true, "item": item }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS nb; ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE nb.notebooks (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, title TEXT NOT NULL, summary TEXT, scope_json TEXT NOT NULL DEFAULT '{}', created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), updated_at TEXT);
             CREATE TABLE nb.notebook_items (id INTEGER PRIMARY KEY AUTOINCREMENT, notebook_id INTEGER NOT NULL, kind TEXT NOT NULL DEFAULT 'note', title TEXT, body_md TEXT NOT NULL DEFAULT '', meta_json TEXT, pinned INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
             CREATE TABLE nb.notebook_sources (id INTEGER PRIMARY KEY AUTOINCREMENT, notebook_id INTEGER NOT NULL, kind TEXT NOT NULL, ref TEXT, label TEXT, body_md TEXT, enabled INTEGER NOT NULL DEFAULT 1, meta_json TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    fn a_notebook(c: &Connection, title: &str) -> i64 {
        c.execute("INSERT INTO nb.notebooks (title) VALUES (?)", [title]).unwrap();
        c.last_insert_rowid()
    }

    #[test]
    fn id_is_required() {
        let c = setup();
        let e = notebookQuiz(&json!({}), &c).unwrap_err();
        assert_eq!(e, "notebook-quiz: id is required");
        let e2 = notebookQuiz(&json!({ "id": 0 }), &c).unwrap_err();
        assert_eq!(e2, "notebook-quiz: id is required");
    }

    #[test]
    fn unknown_notebook_returns_ok_false() {
        let c = setup();
        let r = notebookQuiz(&json!({ "id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "notebook 999 not found" }));
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_and_degrades_gracefully() {
        // proves every deterministic gate (id guard, notebook lookup, corpus resolve, prompt
        // ensure) ran cleanly before the deferred aiCall boundary; the deferred stub's Err is
        // caught (never propagated), matching the JS try/catch degrade-gracefully contract.
        let c = setup();
        let nid = a_notebook(&c, "My Notebook");
        let out = notebookQuiz(&json!({ "id": nid }), &c).unwrap();
        assert_eq!(out["ok"], json!(false));
        assert!(out["error"].as_str().unwrap().starts_with("notebook-quiz AI failed:"));
        // the prompt WAS ensured (ensure_prompt ran) before the deferred call.
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'notebook.quiz.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // no item was inserted — the wave never reached insertItem.
        let items: i64 = c.query_row("SELECT COUNT(*) FROM nb.notebook_items", [], |r| r.get(0)).unwrap();
        assert_eq!(items, 0);
    }

    // ── sanitize (pure, unreachable past the deferred boundary — unit-tested directly) ───────
    #[test]
    fn sanitize_keeps_valid_questions_and_defaults_answer_to_0() {
        let obj = json!({ "questions": [
            { "q": "What is 2+2?", "options": ["3", "4", "5", "6"], "answer": 1, "why": "basic math" },
        ]});
        let out = sanitize(&obj);
        assert_eq!(out, vec![json!({ "q": "What is 2+2?", "options": ["3", "4", "5", "6"], "answer": 1, "why": "basic math" })]);
    }

    #[test]
    fn sanitize_falls_back_to_question_and_correct_aliases() {
        let obj = json!({ "questions": [
            { "question": "Q?", "options": ["a", "b"], "correct": 1 },
        ]});
        let out = sanitize(&obj);
        assert_eq!(out[0]["q"], json!("Q?"));
        assert_eq!(out[0]["answer"], json!(1));
        assert_eq!(out[0]["why"], json!(""));
    }

    #[test]
    fn sanitize_drops_questions_with_empty_q_or_fewer_than_2_options() {
        let obj = json!({ "questions": [
            { "q": "", "options": ["a", "b"] },
            { "q": "only one option", "options": ["a"] },
            { "q": "no options" },
        ]});
        assert_eq!(sanitize(&obj), Vec::<Value>::new());
    }

    #[test]
    fn sanitize_defaults_answer_when_out_of_range_or_not_integer() {
        let obj = json!({ "questions": [
            { "q": "Q1", "options": ["a", "b"], "answer": 5 },
            { "q": "Q2", "options": ["a", "b"], "answer": -1 },
            { "q": "Q3", "options": ["a", "b"], "answer": 1.5 },
        ]});
        let out = sanitize(&obj);
        assert_eq!(out[0]["answer"], json!(0));
        assert_eq!(out[1]["answer"], json!(0));
        assert_eq!(out[2]["answer"], json!(0));
    }

    #[test]
    fn sanitize_caps_questions_at_12_options_at_6_and_why_at_240_chars() {
        let one_q = |i: i64| json!({ "q": format!("Q{i}"), "options": ["a", "b"] });
        let qs: Vec<Value> = (0..20).map(one_q).collect();
        let obj = json!({ "questions": qs });
        assert_eq!(sanitize(&obj).len(), 12);

        let many_opts: Vec<Value> = (0..10).map(|i| json!(format!("opt{i}"))).collect();
        let obj2 = json!({ "questions": [{ "q": "Q", "options": many_opts }] });
        assert_eq!(sanitize(&obj2)[0]["options"].as_array().unwrap().len(), 6);

        let long_why = "x".repeat(300);
        let obj3 = json!({ "questions": [{ "q": "Q", "options": ["a", "b"], "why": long_why }] });
        assert_eq!(sanitize(&obj3)[0]["why"].as_str().unwrap().chars().count(), 240);
    }

    #[test]
    fn sanitize_filters_blank_options_and_trims_whitespace() {
        let obj = json!({ "questions": [{ "q": "  Q  ", "options": ["  a ", "", "b", "   "] }] });
        let out = sanitize(&obj);
        assert_eq!(out[0]["q"], json!("Q"));
        assert_eq!(out[0]["options"], json!(["a", "b"]));
    }
}
