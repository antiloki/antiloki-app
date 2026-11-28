//! book-run.rs — twin of skills/agents/in/book-run.mjs (db, cross-db knowledge.*/brain.*). NAMES
//! ARE CONTRACT: default `bookRun`. The IMPORT TECHNICAL BOOK mission engine: `start` opens (or
//! resumes) a run over a book's decomposed blocks; `step` imports the NEXT block via one wave of
//! THREE scoped faculty calls (Librarian / Quality Warden / Synthesizer) riding rolling context
//! (recent snapshots whole, older ones as one-liners); `pause`/`resume`/`status` manage the run.
//! The COST GOVERNOR flips the run to `cost_break` before a wave that would exceed `budget_usd`.
//!
//! PARITY-DEFERRED: the three faculty calls each end in `crate::ai_ai_call::aiCall` (the deferred
//! network-AI stub — see ai-call.rs's module doc). Everything that surrounds them is ported
//! faithfully and IS the deterministic surface these tests exercise: op guards/validation, the
//! cost-break gate, the rolling-context assembly (`prior_context`, its own private fn, unit-tested
//! directly since aiCall's Err short-circuits `step` before that string is ever used downstream),
//! prompt-template ensure/bootstrap (`ensure_faculty`, reusing the already-converted
//! `ai_prompt_find`/`ai_prompt_add` twins), and all book_runs/book_snapshots bookkeeping up to the
//! point a real wave's output would need parsing. The response-parsing helpers (`parse_arr`,
//! `parse_obj`) and the post-wave staging/press/synopsis logic are ported byte-for-byte from the JS
//! but — like book-ingest.rs's `ingest_file` real-fs branch — are unreachable by this file's own
//! fixture because they run only after a successful `aiCall`, which the deferred stub never
//! produces; correctness there is by inspection + the same helpers' shape as the reachable code.
//! `paper-store` reused via the already-converted `agents_in_paper_store::paperStore` twin.
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
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
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
fn is_int(f: f64) -> bool { f.is_finite() && f.fract() == 0.0 }
fn first(rows: &Value) -> Option<Value> { rows.as_array().and_then(|a| a.first()).cloned() }
fn with_status(run: &Value, status: &str) -> Value {
    let mut o = run.as_object().cloned().unwrap_or_default();
    o.insert("status".into(), json!(status));
    Value::Object(o)
}

const LIBRARIAN_PROMPT: &str = "book.librarian.v1";
const WARDEN_PROMPT: &str = "book.qualitywarden.v1";
const SYNTH_PROMPT: &str = "book.synthesizer.v1";

const LIBRARIAN_MD: &str = r#"You are ◈ THE LIBRARIAN of the book faculty — one lens: memory. You write the module's snapshot and keep its concepts. Nothing else.
THE IMPORT'S GOAL: {{goal}}
THE BOOK SO FAR (living synopsis): {{synopsis}}
PREVIOUS SNAPSHOTS (rolling context): {{prior}}
CURRENT MODULE — {{block_title}}:
{{block_text}}
Respond with COMPLETE JSON only (both keys, never stop mid-array):
{"snapshot_md": "<=120 words — what THIS module teaches, dense; the next wave rides on it",
 "concepts": [{"term": "", "definition": "one line, the book's sense"}]}
THE LIBRARIAN'S OBLIGATION: a teaching module MUST keep 3-5 concepts — a named pattern/technique/term IS a concept. concepts may be [] ONLY for pure code listings or front matter."#;

const WARDEN_MD: &str = r#"You are ⚖ THE QUALITY WARDEN of the book faculty — adversarial, one job: judge THIS module import.
THE MODULE (head): {{block_head}}
THE LIBRARIAN'S SNAPSHOT: {{snapshot}}
THE CONCEPTS KEPT: {{concepts}}
JSON only: {"quality": {"clarity": 0-100, "applicability": 0-100, "note": "one hard line — is this snapshot faithful and usable for THIS engineering system?"}}"#;

const SYNTH_MD: &str = r#"You are ⚗ THE SYNTHESIZER of the book faculty — one lens: what the system can ENFORCE or REUSE. Laws and skills only when the module truly carries one; [] is honest.
THE IMPORT'S GOAL: {{goal}}
THE MODULE — {{block_title}}:
{{block_text}}
THE LIBRARIAN'S SNAPSHOT: {{snapshot}}
Respond with COMPLETE JSON only (both keys):
{"laws": [{"key": "BOOK-<SLUG>-NN", "statement": "one enforceable rule", "why": "", "example": "", "antipattern": ""}],
 "skills": [{"name": "", "description": "one when-to-use line", "body_md": "steps + pitfalls"}]}
laws <=3 (only rules worth ratifying), skills <=2 (only genuinely reusable methods)."#;

fn faculty_prompt(name: &str) -> (&'static str, i64, Value) {
    match name {
        LIBRARIAN_PROMPT => (
            LIBRARIAN_MD,
            1600,
            json!([
                { "field_name": "snapshot_md", "field_type": "string", "required": true },
                { "field_name": "concepts", "field_type": "string", "required": true },
            ]),
        ),
        WARDEN_PROMPT => (
            WARDEN_MD,
            220,
            json!([{ "field_name": "quality", "field_type": "string", "required": true }]),
        ),
        _ => (
            SYNTH_MD,
            1200,
            json!([
                { "field_name": "laws", "field_type": "string", "required": true },
                { "field_name": "skills", "field_type": "string", "required": true },
            ]),
        ),
    }
}

// try promptFind; on miss, promptAdd then promptFind again (mirrors JS ensureFaculty's try/catch).
fn ensure_faculty(name: &str, db: &Connection) -> Result<(), String> {
    if crate::ai_prompt_find::promptFind(&json!({ "name": name }), db).is_ok() {
        return Ok(());
    }
    let (md, max, fields) = faculty_prompt(name);
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": name,
            "template_md": md,
            "default_tier": "mid",
            "default_temperature": "cold",
            "max_output_tokens": max,
            "output_fields": fields,
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!({ "name": name }), db)?;
    Ok(())
}

// { field_name: value_text } built from an ai_calls response's output_kv rows.
fn kv_out(res: &Value) -> Value {
    let mut o = Map::new();
    if let Some(arr) = res.get("output_kv").and_then(|v| v.as_array()) {
        for row in arr {
            if let Some(field) = row.get("field_name").and_then(|v| v.as_str()) {
                if let Some(val) = row.get("value_text") {
                    if !val.is_null() { o.insert(field.to_string(), val.clone()); }
                }
            }
        }
    }
    Value::Object(o)
}

struct FacResult { out: Value, ai_call_id: Value, cost: f64 }

// PARITY-DEFERRED past ensure_faculty: dispatches through crate::ai_ai_call::aiCall, which always
// returns Err (see that module's doc) — so this always propagates an Err once the (real, ported)
// prompt-ensure step above it has run.
fn fac_call(name: &str, kv: Value, provider_fn: &Value, intent: &str, db: &Connection) -> Result<FacResult, String> {
    ensure_faculty(name, db)?;
    let input = json!({ "prompt_name": name, "input_kv": kv, "provider_fn": provider_fn, "intent": intent });
    let res = crate::ai_ai_call::aiCall(&input, db)?;
    let out = kv_out(&res);
    let ai_call_id = res.get("ai_call_id").cloned().unwrap_or(Value::Null);
    let cost = res.get("cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
    Ok(FacResult { out, ai_call_id, cost })
}

// `JSON.parse(String(s ?? '[]'))`, Array.isArray guard, `[]` on any failure.
fn parse_arr(v: Option<&Value>) -> Vec<Value> {
    let s = match v { None | Some(Value::Null) => "[]".to_string(), Some(x) => js_string(x) };
    match serde_json::from_str::<Value>(&s) {
        Ok(Value::Array(a)) => a,
        _ => vec![],
    }
}
// `JSON.parse(String(s ?? '{}'))`, typeof === 'object' guard, `{}` on any failure.
fn parse_obj(v: Option<&Value>) -> Value {
    let s = match v { None | Some(Value::Null) => "{}".to_string(), Some(x) => js_string(x) };
    match serde_json::from_str::<Value>(&s) {
        Ok(o @ Value::Object(_)) => o,
        _ => json!({}),
    }
}

fn touch(db: &Connection, run_id: i64) -> Result<(), String> {
    crate::db_json::run_write(db, "UPDATE knowledge.book_runs SET updated_at = datetime('now') WHERE id = ?", &[json!(run_id)])?;
    Ok(())
}

// ROLLING CONTEXT — the last 3 snapshots whole (capped 1300 chars each), everything older as
// one-liners (first line, capped 130 chars), the whole thing capped to the trailing 7000 chars.
// Own private fn (mirrors book-run.mjs's inline block) so it is directly unit-testable —
// deterministic prompt/context assembly, per the crossing playbook's coverage rule.
fn prior_context(db: &Connection, book_id: i64, cursor: i64) -> Result<String, String> {
    let snaps_rows = crate::db_json::query_rows(
        db,
        "SELECT block_ord, snapshot_md FROM knowledge.book_snapshots WHERE book_id = ? AND block_ord < ? ORDER BY block_ord",
        &[json!(book_id), json!(cursor)],
    )?;
    let snaps: Vec<Value> = snaps_rows.as_array().cloned().unwrap_or_default();
    let n = snaps.len();
    let split = n.saturating_sub(3);
    let older = &snaps[..split];
    let recent = &snaps[split..];
    let mut lines: Vec<String> = Vec::new();
    for s in older {
        let ord = js_string(s.get("block_ord").unwrap_or(&Value::Null));
        let first_line: String = js_string(s.get("snapshot_md").unwrap_or(&Value::Null))
            .split('\n').next().unwrap_or("").chars().take(130).collect();
        lines.push(format!("· [{ord}] {first_line}"));
    }
    for s in recent {
        let ord = js_string(s.get("block_ord").unwrap_or(&Value::Null));
        let snap: String = js_string(s.get("snapshot_md").unwrap_or(&Value::Null)).chars().take(1300).collect();
        lines.push(format!("[{ord}]\n{snap}"));
    }
    let joined = lines.join("\n");
    if joined.is_empty() {
        return Ok("(this is the first block)".to_string());
    }
    let chars: Vec<char> = joined.chars().collect();
    let start = chars.len().saturating_sub(7000);
    Ok(chars[start..].iter().collect())
}

pub fn bookRun(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent). provider_fn is a JS function
    // reference threaded opaquely into aiCall's input for provider selection/DI — there is no Rust
    // equivalent to carry (aiCall's dispatch itself is the deferred boundary), so it is passed as
    // `null` into the constructed aiCall input below.
    let provider_fn = Value::Null;
    let op = str_qq(input.get("op"));
    let op = if op.is_empty() { "status".to_string() } else { op };

    if op == "start" {
        let book_id_num = js_number(input.get("book_id"));
        if !is_int(book_id_num) || book_id_num <= 0.0 {
            return Err("book-run: book_id is required".to_string());
        }
        let book_id = book_id_num as i64;
        let book_rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.books WHERE id = ?", &[json!(book_id)])?;
        let Some(_book) = first(&book_rows) else {
            return Err(format!("book-run: book '{book_id}' not found"));
        };
        let goal = str_qq(input.get("goal")).trim().to_string();
        if !goal.is_empty() {
            crate::db_json::run_write(db, "UPDATE knowledge.books SET goal = ? WHERE id = ?", &[json!(goal), json!(book_id)])?;
        }
        let open_rows = crate::db_json::query_rows(
            db,
            "SELECT * FROM knowledge.book_runs WHERE book_id = ? AND status IN ('running','paused','cost_break') ORDER BY id DESC LIMIT 1",
            &[json!(book_id)],
        )?;
        if let Some(open) = first(&open_rows) {
            return Ok(json!({ "ok": true, "run": open, "resumed": true }));
        }
        let budget_num = js_number(input.get("budget_usd"));
        let budget_param = if nz(input.get("budget_usd")).is_some() && budget_num.is_finite() && budget_num > 0.0 {
            json!(budget_num)
        } else {
            Value::Null
        };
        let done_rows = crate::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM knowledge.book_snapshots WHERE book_id = ?", &[json!(book_id)])?;
        let done = first(&done_rows).and_then(|r| r.get("n").and_then(|v| v.as_i64())).unwrap_or(0);
        crate::db_json::run_write(
            db,
            "INSERT INTO knowledge.book_runs (book_id, status, budget_usd, cursor) VALUES (?, 'running', ?, ?)",
            &[json!(book_id), budget_param, json!(done)],
        )?;
        let run_id = db.last_insert_rowid();
        let run_rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.book_runs WHERE id = ?", &[json!(run_id)])?;
        return Ok(json!({ "ok": true, "run": first(&run_rows).unwrap_or(Value::Null) }));
    }

    let run_id_num = js_number(input.get("run_id"));
    if !is_int(run_id_num) || run_id_num <= 0.0 {
        return Err("book-run: run_id is required".to_string());
    }
    let run_id = run_id_num as i64;
    let run_rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.book_runs WHERE id = ?", &[json!(run_id)])?;
    let Some(run) = first(&run_rows) else {
        return Err(format!("book-run: run '{run_id}' not found"));
    };

    if op == "status" {
        return Ok(json!({ "ok": true, "run": run }));
    }
    if op == "pause" {
        crate::db_json::run_write(db, "UPDATE knowledge.book_runs SET status = 'paused' WHERE id = ?", &[json!(run_id)])?;
        touch(db, run_id)?;
        return Ok(json!({ "ok": true, "run": with_status(&run, "paused") }));
    }
    if op == "resume" {
        let budget_num = js_number(input.get("budget_usd"));
        if nz(input.get("budget_usd")).is_some() && budget_num.is_finite() && budget_num > 0.0 {
            crate::db_json::run_write(db, "UPDATE knowledge.book_runs SET budget_usd = ? WHERE id = ?", &[json!(budget_num), json!(run_id)])?;
        }
        crate::db_json::run_write(
            db,
            "UPDATE knowledge.book_runs SET status = 'running' WHERE id = ? AND status IN ('paused','cost_break')",
            &[json!(run_id)],
        )?;
        touch(db, run_id)?;
        let fresh = crate::db_json::query_rows(db, "SELECT * FROM knowledge.book_runs WHERE id = ?", &[json!(run_id)])?;
        return Ok(json!({ "ok": true, "run": first(&fresh).unwrap_or(Value::Null) }));
    }
    if op != "step" {
        return Err("book-run: op 'start'|'step'|'pause'|'resume'|'status' is required".to_string());
    }

    let run_status = run.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if run_status != "running" {
        return Ok(json!({ "ok": false, "run": run, "error": format!("the run is {run_status} — resume it first") }));
    }
    let book_id = run.get("book_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let book_rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.books WHERE id = ?", &[json!(book_id)])?;
    let book = first(&book_rows);
    let total = book.as_ref().and_then(|b| b.get("blocks")).and_then(|v| v.as_i64()).unwrap_or(0);
    let cursor = run.get("cursor").and_then(|v| v.as_i64()).unwrap_or(0);
    if cursor >= total {
        crate::db_json::run_write(db, "UPDATE knowledge.book_runs SET status = 'done' WHERE id = ?", &[json!(run_id)])?;
        return Ok(json!({ "ok": true, "run": with_status(&run, "done"), "done": true }));
    }
    // the COST BREAK — checked before spending, so the break is never overshot by a full wave
    let budget_usd = run.get("budget_usd").cloned().unwrap_or(Value::Null);
    let spent_usd = run.get("spent_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
    if let Some(budget_f) = budget_usd.as_f64() {
        if spent_usd >= budget_f {
            crate::db_json::run_write(db, "UPDATE knowledge.book_runs SET status = 'cost_break' WHERE id = ?", &[json!(run_id)])?;
            let msg = format!(
                "cost break — ${:.4} of the ${:.2} budget spent; resume with a higher budget to continue",
                spent_usd, budget_f
            );
            return Ok(json!({ "ok": false, "run": with_status(&run, "cost_break"), "error": msg }));
        }
    }

    let block_rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.book_blocks WHERE book_id = ? AND ord = ?", &[json!(book_id), json!(cursor)])?;
    let Some(block) = first(&block_rows) else {
        crate::db_json::run_write(db, "UPDATE knowledge.book_runs SET status = 'error', error = 'block missing' WHERE id = ?", &[json!(run_id)])?;
        return Ok(json!({ "ok": false, "error": format!("block {cursor} is missing") }));
    };
    let block_title = js_string(block.get("title").unwrap_or(&Value::Null));
    let block_text_full = js_string(block.get("text").unwrap_or(&Value::Null));

    let prior = prior_context(db, book_id, cursor)?;

    let goal_val = {
        let g = js_string(book.as_ref().and_then(|b| b.get("goal")).unwrap_or(&Value::Null));
        if g.is_empty() { "import this book’s reusable engineering knowledge".to_string() } else { g }
    };
    let synopsis_val = {
        let s = js_string(book.as_ref().and_then(|b| b.get("synopsis_md")).unwrap_or(&Value::Null));
        if s.is_empty() { "(not written yet — this block seeds it)".to_string() } else { s }
    };

    // ── the FACULTY runs: three real agents, three real calls, three receipts. PARITY-DEFERRED
    // from here on (see module doc) — the first `fac_call` propagates aiCall's Err.
    let lib_kv = json!([
        { "key": "goal", "value": goal_val },
        { "key": "synopsis", "value": synopsis_val },
        { "key": "prior", "value": prior },
        { "key": "block_title", "value": block_title },
        { "key": "block_text", "value": block_text_full.chars().take(14000).collect::<String>() },
    ]);
    let lib = fac_call(LIBRARIAN_PROMPT, lib_kv, &provider_fn, "system.book.run", db)?;
    let snapshot = js_string(lib.out.get("snapshot_md").unwrap_or(&Value::Null)).trim().to_string();
    if snapshot.is_empty() {
        crate::db_json::run_write(db, "UPDATE knowledge.book_runs SET status = 'error', error = 'the librarian returned no snapshot' WHERE id = ?", &[json!(run_id)])?;
        return Ok(json!({ "ok": false, "error": "the librarian returned no snapshot", "ai_call_id": lib.ai_call_id }));
    }

    let ward_kv = json!([
        { "key": "block_head", "value": block_text_full.chars().take(900).collect::<String>() },
        { "key": "snapshot", "value": snapshot.chars().take(1200).collect::<String>() },
        { "key": "concepts", "value": js_string(lib.out.get("concepts").unwrap_or(&json!("[]"))).chars().take(900).collect::<String>() },
    ]);
    let ward = fac_call(WARDEN_PROMPT, ward_kv, &provider_fn, "system.book.run", db)?;
    let quality = parse_obj(ward.out.get("quality"));

    let goal_val2 = {
        let g = js_string(book.as_ref().and_then(|b| b.get("goal")).unwrap_or(&Value::Null));
        if g.is_empty() { "import this book’s reusable engineering knowledge".to_string() } else { g }
    };
    let synth_kv = json!([
        { "key": "goal", "value": goal_val2 },
        { "key": "block_title", "value": block_title },
        { "key": "block_text", "value": block_text_full.chars().take(12000).collect::<String>() },
        { "key": "snapshot", "value": snapshot.chars().take(1200).collect::<String>() },
    ]);
    let synth = fac_call(SYNTH_PROMPT, synth_kv, &provider_fn, "system.book.run", db)?;

    let cost = lib.cost + ward.cost + synth.cost;
    let concepts_out = lib.out.get("concepts").cloned();
    let laws_out = synth.out.get("laws").cloned();
    let skills_out = synth.out.get("skills").cloned();
    let agents_meta = json!({
        "librarian": { "ai_call_id": lib.ai_call_id, "cost_usd": lib.cost },
        "warden": { "ai_call_id": ward.ai_call_id, "cost_usd": ward.cost },
        "synthesizer": { "ai_call_id": synth.ai_call_id, "cost_usd": synth.cost },
    });
    let mut quality_with_agents = quality.as_object().cloned().unwrap_or_default();
    quality_with_agents.insert("agents".into(), agents_meta.clone());
    crate::db_json::run_write(
        db,
        "INSERT OR REPLACE INTO knowledge.book_snapshots (book_id, block_ord, snapshot_md, quality_json, ai_call_id, cost_usd) VALUES (?, ?, ?, ?, ?, ?)",
        &[json!(book_id), json!(cursor), json!(snapshot), json!(serde_json::to_string(&Value::Object(quality_with_agents)).map_err(|e| e.to_string())?), lib.ai_call_id.clone(), json!(cost)],
    )?;
    let book_title = js_string(book.as_ref().and_then(|b| b.get("title")).unwrap_or(&json!("book")));
    let book_title = if book_title.is_empty() || book_title == "null" { "book".to_string() } else { book_title };
    let refstr = format!("{book_title} · {block_title}");

    let mut staged_concepts = 0i64;
    let mut staged_laws = 0i64;
    let mut staged_skills = 0i64;
    for c in parse_arr(concepts_out.as_ref()).into_iter().take(5) {
        let term = c.get("term").filter(|v| !v.is_null());
        let def = c.get("definition").filter(|v| !v.is_null());
        let (Some(term), Some(def)) = (term, def) else { continue };
        let term_s = js_string(term);
        let exists = crate::db_json::query_rows(db, "SELECT 1 FROM knowledge.book_concepts WHERE book_id = ? AND LOWER(term) = LOWER(?)", &[json!(book_id), json!(term_s)])?;
        if first(&exists).is_some() { continue; }
        let term_t: String = term_s.chars().take(120).collect();
        let def_t: String = js_string(def).chars().take(400).collect();
        crate::db_json::run_write(db, "INSERT INTO knowledge.book_concepts (book_id, block_ord, term, definition, ref) VALUES (?, ?, ?, ?, ?)", &[json!(book_id), json!(cursor), json!(term_t), json!(def_t), json!(refstr)])?;
        staged_concepts += 1;
    }
    for l in parse_arr(laws_out.as_ref()).into_iter().take(3) {
        let key = l.get("key").filter(|v| !v.is_null());
        let statement = l.get("statement").filter(|v| !v.is_null());
        let (Some(key), Some(statement)) = (key, statement) else { continue };
        let key_t: String = js_string(key).chars().take(80).collect();
        let exists = crate::db_json::query_rows(db, "SELECT 1 FROM knowledge.book_law_drafts WHERE book_id = ? AND key = ?", &[json!(book_id), json!(key_t.clone())])?;
        if first(&exists).is_some() { continue; }
        let statement_t: String = js_string(statement).chars().take(600).collect();
        let why_t: String = js_string(l.get("why").unwrap_or(&json!(""))).chars().take(600).collect();
        let example_t: String = js_string(l.get("example").unwrap_or(&json!(""))).chars().take(600).collect();
        let anti_t: String = js_string(l.get("antipattern").unwrap_or(&json!(""))).chars().take(600).collect();
        crate::db_json::run_write(
            db,
            "INSERT INTO knowledge.book_law_drafts (book_id, block_ord, key, statement, why, example, antipattern, ref) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            &[json!(book_id), json!(cursor), json!(key_t), json!(statement_t), json!(why_t), json!(example_t), json!(anti_t), json!(refstr)],
        )?;
        staged_laws += 1;
    }
    for s in parse_arr(skills_out.as_ref()).into_iter().take(2) {
        let name = s.get("name").filter(|v| !v.is_null());
        let Some(name) = name else { continue };
        let name_s = js_string(name);
        let exists = crate::db_json::query_rows(db, "SELECT 1 FROM knowledge.book_skill_drafts WHERE book_id = ? AND LOWER(name) = LOWER(?)", &[json!(book_id), json!(name_s.clone())])?;
        if first(&exists).is_some() { continue; }
        let name_t: String = name_s.chars().take(120).collect();
        let desc_t: String = js_string(s.get("description").unwrap_or(&json!(""))).chars().take(300).collect();
        let body_t: String = js_string(s.get("body_md").unwrap_or(&json!(""))).chars().take(6000).collect();
        crate::db_json::run_write(
            db,
            "INSERT INTO knowledge.book_skill_drafts (book_id, block_ord, name, description, body_md, ref) VALUES (?, ?, ?, ?, ?, ?)",
            &[json!(book_id), json!(cursor), json!(name_t), json!(desc_t), json!(body_t), json!(refstr)],
        )?;
        staged_skills += 1;
    }
    let staged = json!({ "concepts": staged_concepts, "laws": staged_laws, "skills": staged_skills });

    // MODULE PAPER — garnish: its failure never blocks the wave that already landed.
    let title = format!("«{book_title}» — module {}: {block_title}", cursor + 1);
    let title: String = title.chars().take(200).collect();
    let goal_for_paper = js_string(book.as_ref().and_then(|b| b.get("goal")).unwrap_or(&json!("")));
    let _ = crate::agents_in_paper_store::paperStore(
        &json!({
            "op": "press",
            "cite_id": format!("book-{book_id}-m{cursor}"),
            "kind": "book-module",
            "subject": format!("book:{book_id}"),
            "snapshot_id": format!("m{cursor}"),
            "title": title,
            "findings": { "block_ord": cursor, "block_title": block_title, "snapshot_md": snapshot, "staged": staged, "cost_usd": cost, "goal": goal_for_paper },
        }),
        db,
    );

    let synopsis_md = js_string(book.as_ref().and_then(|b| b.get("synopsis_md")).unwrap_or(&Value::Null));
    if cursor == 0 || synopsis_md.trim().is_empty() {
        let snap_t: String = snapshot.chars().take(1200).collect();
        crate::db_json::run_write(db, "UPDATE knowledge.books SET synopsis_md = ? WHERE id = ?", &[json!(snap_t), json!(book_id)])?;
    }

    let spent = spent_usd + cost;
    let next_cursor = cursor + 1;
    let status = if next_cursor >= total {
        "done"
    } else if let Some(budget_f) = budget_usd.as_f64() {
        if spent >= budget_f { "cost_break" } else { "running" }
    } else {
        "running"
    };
    crate::db_json::run_write(
        db,
        "UPDATE knowledge.book_runs SET cursor = ?, spent_usd = ?, status = ?, updated_at = datetime('now') WHERE id = ?",
        &[json!(next_cursor), json!(spent), json!(status), json!(run_id)],
    )?;
    let fresh_run = crate::db_json::query_rows(db, "SELECT * FROM knowledge.book_runs WHERE id = ?", &[json!(run_id)])?;
    Ok(json!({
        "ok": true,
        "run": first(&fresh_run).unwrap_or(Value::Null),
        "wave": { "block_ord": cursor, "title": block_title, "snapshot_md": snapshot, "quality": quality, "staged": staged, "cost_usd": cost, "ai_call_id": lib.ai_call_id, "agents": agents_meta },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS knowledge; ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE knowledge.books (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, author TEXT, goal TEXT NOT NULL DEFAULT '', synopsis_md TEXT NOT NULL DEFAULT '', source_kind TEXT, tokens_est INTEGER, blocks INTEGER NOT NULL DEFAULT 0, tier TEXT, created_at TEXT);
             CREATE TABLE knowledge.book_blocks (id INTEGER PRIMARY KEY AUTOINCREMENT, book_id INTEGER NOT NULL, ord INTEGER NOT NULL, title TEXT, text TEXT, tokens_est INTEGER);
             CREATE TABLE knowledge.book_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, book_id INTEGER NOT NULL, status TEXT NOT NULL DEFAULT 'running', budget_usd REAL, spent_usd REAL NOT NULL DEFAULT 0, cursor INTEGER NOT NULL DEFAULT 0, error TEXT, created_at TEXT NOT NULL DEFAULT (datetime('now')), updated_at TEXT NOT NULL DEFAULT (datetime('now')));
             CREATE TABLE knowledge.book_snapshots (id INTEGER PRIMARY KEY AUTOINCREMENT, book_id INTEGER NOT NULL, block_ord INTEGER NOT NULL, snapshot_md TEXT NOT NULL, quality_json TEXT NOT NULL DEFAULT '{}', ai_call_id INTEGER, cost_usd REAL, created_at TEXT, UNIQUE(book_id, block_ord));
             CREATE TABLE knowledge.book_concepts (id INTEGER PRIMARY KEY AUTOINCREMENT, book_id INTEGER NOT NULL, block_ord INTEGER, term TEXT, definition TEXT, ref TEXT);
             CREATE TABLE knowledge.book_law_drafts (id INTEGER PRIMARY KEY AUTOINCREMENT, book_id INTEGER NOT NULL, block_ord INTEGER, key TEXT, statement TEXT, why TEXT, example TEXT, antipattern TEXT, ref TEXT, status TEXT NOT NULL DEFAULT 'draft');
             CREATE TABLE knowledge.book_skill_drafts (id INTEGER PRIMARY KEY AUTOINCREMENT, book_id INTEGER NOT NULL, block_ord INTEGER, name TEXT, description TEXT, body_md TEXT, ref TEXT, status TEXT NOT NULL DEFAULT 'draft');
             CREATE TABLE knowledge.papers (id INTEGER PRIMARY KEY AUTOINCREMENT, cite_id TEXT NOT NULL UNIQUE, kind TEXT NOT NULL DEFAULT 'folder', subject TEXT NOT NULL, snapshot_id INTEGER, title TEXT NOT NULL, findings_json TEXT NOT NULL DEFAULT '{}', created_at TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    fn a_book(c: &Connection, blocks: i64) -> i64 {
        c.execute("INSERT INTO knowledge.books (title, blocks) VALUES ('The Book', ?)", [blocks]).unwrap();
        c.last_insert_rowid()
    }

    #[test]
    fn start_requires_book_id_and_book_existing() {
        let c = setup();
        assert!(bookRun(&json!({ "op": "start" }), &c).unwrap_err().contains("book_id is required"));
        assert!(bookRun(&json!({ "op": "start", "book_id": 999 }), &c).unwrap_err().contains("book '999' not found"));
    }

    #[test]
    fn start_creates_running_run_with_cursor_from_snapshots() {
        let c = setup();
        let bid = a_book(&c, 3);
        c.execute("INSERT INTO knowledge.book_snapshots (book_id, block_ord, snapshot_md) VALUES (?, 0, 's0')", [bid]).unwrap();
        let r = bookRun(&json!({ "op": "start", "book_id": bid, "budget_usd": 5.5, "goal": "  learn things  " }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["run"]["status"], json!("running"));
        assert_eq!(r["run"]["cursor"], json!(1));
        assert_eq!(r["run"]["budget_usd"], json!(5.5));
        let goal: String = c.query_row("SELECT goal FROM knowledge.books WHERE id = ?", [bid], |row| row.get(0)).unwrap();
        assert_eq!(goal, "learn things");
    }

    #[test]
    fn start_resumes_an_open_run_instead_of_creating_a_new_one() {
        let c = setup();
        let bid = a_book(&c, 3);
        c.execute("INSERT INTO knowledge.book_runs (book_id, status, cursor) VALUES (?, 'paused', 1)", [bid]).unwrap();
        let r = bookRun(&json!({ "op": "start", "book_id": bid }), &c).unwrap();
        assert_eq!(r["resumed"], json!(true));
        assert_eq!(r["run"]["status"], json!("paused"));
        let count: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.book_runs", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn start_ignores_non_positive_budget() {
        let c = setup();
        let bid = a_book(&c, 3);
        let r = bookRun(&json!({ "op": "start", "book_id": bid, "budget_usd": -5 }), &c).unwrap();
        assert_eq!(r["run"]["budget_usd"], Value::Null);
    }

    #[test]
    fn run_id_guard_and_not_found() {
        let c = setup();
        assert!(bookRun(&json!({ "op": "status" }), &c).unwrap_err().contains("run_id is required"));
        assert!(bookRun(&json!({ "op": "status", "run_id": 42 }), &c).unwrap_err().contains("run '42' not found"));
    }

    #[test]
    fn status_pause_resume_roundtrip() {
        let c = setup();
        let bid = a_book(&c, 3);
        c.execute("INSERT INTO knowledge.book_runs (book_id, status, cursor) VALUES (?, 'running', 0)", [bid]).unwrap();
        let rid = c.last_insert_rowid();
        let st = bookRun(&json!({ "op": "status", "run_id": rid }), &c).unwrap();
        assert_eq!(st["run"]["status"], json!("running"));
        let p = bookRun(&json!({ "op": "pause", "run_id": rid }), &c).unwrap();
        assert_eq!(p["run"]["status"], json!("paused"));
        let db_status: String = c.query_row("SELECT status FROM knowledge.book_runs WHERE id = ?", [rid], |row| row.get(0)).unwrap();
        assert_eq!(db_status, "paused");
        let r = bookRun(&json!({ "op": "resume", "run_id": rid, "budget_usd": 9 }), &c).unwrap();
        assert_eq!(r["run"]["status"], json!("running"));
        assert_eq!(r["run"]["budget_usd"], json!(9.0));
    }

    #[test]
    fn resume_leaves_status_untouched_when_run_isnt_paused_or_cost_break() {
        let c = setup();
        let bid = a_book(&c, 3);
        c.execute("INSERT INTO knowledge.book_runs (book_id, status, cursor) VALUES (?, 'done', 3)", [bid]).unwrap();
        let rid = c.last_insert_rowid();
        let r = bookRun(&json!({ "op": "resume", "run_id": rid }), &c).unwrap();
        assert_eq!(r["run"]["status"], json!("done"));
    }

    #[test]
    fn unknown_op_errors() {
        let c = setup();
        let bid = a_book(&c, 3);
        c.execute("INSERT INTO knowledge.book_runs (book_id, status, cursor) VALUES (?, 'running', 0)", [bid]).unwrap();
        let rid = c.last_insert_rowid();
        assert!(bookRun(&json!({ "op": "bogus", "run_id": rid }), &c).unwrap_err().contains("op 'start'|'step'"));
    }

    #[test]
    fn step_refuses_when_run_is_not_running() {
        let c = setup();
        let bid = a_book(&c, 3);
        c.execute("INSERT INTO knowledge.book_runs (book_id, status, cursor) VALUES (?, 'paused', 0)", [bid]).unwrap();
        let rid = c.last_insert_rowid();
        let r = bookRun(&json!({ "op": "step", "run_id": rid }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("the run is paused — resume it first"));
    }

    #[test]
    fn step_flips_to_done_when_cursor_reaches_total_blocks() {
        let c = setup();
        let bid = a_book(&c, 2);
        c.execute("INSERT INTO knowledge.book_runs (book_id, status, cursor) VALUES (?, 'running', 2)", [bid]).unwrap();
        let rid = c.last_insert_rowid();
        let r = bookRun(&json!({ "op": "step", "run_id": rid }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["done"], json!(true));
        assert_eq!(r["run"]["status"], json!("done"));
        let db_status: String = c.query_row("SELECT status FROM knowledge.book_runs WHERE id = ?", [rid], |row| row.get(0)).unwrap();
        assert_eq!(db_status, "done");
    }

    #[test]
    fn step_flips_to_cost_break_before_spending_more() {
        let c = setup();
        let bid = a_book(&c, 5);
        c.execute("INSERT INTO knowledge.book_runs (book_id, status, budget_usd, spent_usd, cursor) VALUES (?, 'running', 1.0, 1.0, 0)", [bid]).unwrap();
        let rid = c.last_insert_rowid();
        let r = bookRun(&json!({ "op": "step", "run_id": rid }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["run"]["status"], json!("cost_break"));
        assert!(r["error"].as_str().unwrap().contains("cost break — $1.0000 of the $1.00 budget spent"));
        let db_status: String = c.query_row("SELECT status FROM knowledge.book_runs WHERE id = ?", [rid], |row| row.get(0)).unwrap();
        assert_eq!(db_status, "cost_break");
    }

    #[test]
    fn step_marks_error_when_block_is_missing() {
        let c = setup();
        let bid = a_book(&c, 3); // blocks=3 but no book_blocks rows inserted
        c.execute("INSERT INTO knowledge.book_runs (book_id, status, cursor) VALUES (?, 'running', 0)", [bid]).unwrap();
        let rid = c.last_insert_rowid();
        let r = bookRun(&json!({ "op": "step", "run_id": rid }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("block 0 is missing"));
        let db_status: String = c.query_row("SELECT status FROM knowledge.book_runs WHERE id = ?", [rid], |row| row.get(0)).unwrap();
        assert_eq!(db_status, "error");
    }

    #[test]
    fn step_reaches_the_parity_deferred_aicall_boundary() {
        // proves every deterministic gate (status/cursor/cost-break/block-fetch/rolling-context/
        // prompt-ensure) ran cleanly and the error is aiCall's deferred stub, not an earlier bug.
        let c = setup();
        let bid = a_book(&c, 1);
        c.execute("INSERT INTO knowledge.book_blocks (book_id, ord, title, text) VALUES (?, 0, 'Intro', 'some block text')", [bid]).unwrap();
        c.execute("INSERT INTO knowledge.book_runs (book_id, status, cursor) VALUES (?, 'running', 0)", [bid]).unwrap();
        let rid = c.last_insert_rowid();
        let err = bookRun(&json!({ "op": "step", "run_id": rid }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        // the librarian's prompt template WAS ensured (ensure_faculty ran) before the deferred call
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'book.librarian.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    // ── prior_context (rolling-context assembly) ──────────────────────────────────────────
    #[test]
    fn prior_context_defaults_when_no_snapshots() {
        let c = setup();
        let bid = a_book(&c, 5);
        assert_eq!(prior_context(&c, bid, 0).unwrap(), "(this is the first block)");
    }

    #[test]
    fn prior_context_all_recent_when_three_or_fewer() {
        let c = setup();
        let bid = a_book(&c, 5);
        c.execute("INSERT INTO knowledge.book_snapshots (book_id, block_ord, snapshot_md) VALUES (?, 0, 'first snap')", [bid]).unwrap();
        c.execute("INSERT INTO knowledge.book_snapshots (book_id, block_ord, snapshot_md) VALUES (?, 1, 'second snap')", [bid]).unwrap();
        let p = prior_context(&c, bid, 2).unwrap();
        assert_eq!(p, "[0]\nfirst snap\n[1]\nsecond snap");
    }

    #[test]
    fn prior_context_older_ones_become_one_liners() {
        let c = setup();
        let bid = a_book(&c, 10);
        for i in 0..5 {
            c.execute("INSERT INTO knowledge.book_snapshots (book_id, block_ord, snapshot_md) VALUES (?, ?, ?)", rusqlite::params![bid, i, format!("snap {i}\nsecond line")]).unwrap();
        }
        let p = prior_context(&c, bid, 5).unwrap();
        let expected = "· [0] snap 0\n· [1] snap 1\n[2]\nsnap 2\nsecond line\n[3]\nsnap 3\nsecond line\n[4]\nsnap 4\nsecond line";
        assert_eq!(p, expected);
    }

    #[test]
    fn prior_context_caps_to_trailing_7000_chars() {
        let c = setup();
        let bid = a_book(&c, 2);
        let long = "x".repeat(2000);
        c.execute("INSERT INTO knowledge.book_snapshots (book_id, block_ord, snapshot_md) VALUES (?, 0, ?)", rusqlite::params![bid, long]).unwrap();
        let p = prior_context(&c, bid, 1).unwrap();
        assert!(p.len() <= 7000);
    }
}
