//! book-swarm.rs — twin of skills/agents/in/book-swarm.mjs (db lane, cross-db `knowledge`+`brain`).
//! NAMES ARE CONTRACT: `bookSwarm` (default).
//!
//! P2+P3 of the Authority Atlas: the QUOTA-FILLING swarm. Walks the atlas's receipted slots in a
//! stable order and, per slot, runs ONE single-lens worker call + ONE adversarial warden call, then
//! writes the surviving unit into the IR via the already-converted `knowledge-ir` twin. ops: start
//! { book_id, budget_usd? } · step { run_id, batch?=4, only? } · pause/resume { run_id } · status
//! { run_id?, book_id?, list_kind?, limit? }.
//!
//! PARITY-DEFERRED (the `call` closure's `aiCall` leg only): every worker/warden invocation routes
//! through `crate::ai_ai_call::aiCall`, itself a documented deferred stub (see ai-call.rs's module
//! doc) that ALWAYS returns `Err` — the real network dispatch to a BYOK LLM provider is genuinely
//! non-deterministic and unfixturable in principle. That means the `step` op's per-slot branches
//! (definitions/normatives/questions/tradeoffs/listings/procedures/dialogue) are ported FAITHFULLY
//! (prompt/context assembly, response-shape parsing via `out2obj`/`parseArr`, the warden gate, the
//! `knowledge-ir` `put`/`edge` writes) but can only be exercised natively up to the first real
//! `aiCall` — reached, it Errs, the outer catch persists `status='error'` and returns
//! `{ok:false,error,events}`, exactly mirroring the JS try/catch. The ONE branch requiring no AI
//! call — `glossary` (the book's own back-matter definitions go straight to the IR, $0, `verdict:
//! 'kept'`) — plus the whole deterministic shell (start/pause/resume/status, cost-break gating,
//! cursor/skip bookkeeping, the P4 xref-edge pass, `ensurePrompt`'s find-or-register flow) is
//! exercised end-to-end by the native tests below.
//!
//! Cross-module calls (pre-existing Rust twins, names verified against their own pub signatures):
//! `crate::ai_ai_call::aiCall`, `crate::ai_prompt_find::promptFind`, `crate::ai_prompt_add::promptAdd`,
//! `crate::agents_in_book_atlas::{buildAtlas, applyAnnotations, loadAnnotations}`,
//! `crate::entities_knowledge_ir::knowledgeIr`.
//!
//! Frozen clock 2026-01-01 (inherited from `datetime('now')` SQL defaults — no direct Rust
//! wall-clock reads in this module).
// QUALITY: bespoke-native — the deterministic shell (guards, atlas/annotation merge, cost-break
// bookkeeping, per-type prompt/context assembly, response parsing, IR writes, P4 xref pass) is
// ported and native-tested; the aiCall network dispatch is the disclosed PARITY-DEFERRED boundary
// inherited from ai-call.rs, not re-litigated here.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

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
fn str_qq(v: Option<&Value>) -> String { // = JS String(v ?? '')
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn js_string(v: &Value) -> String { // = JS String(v)
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn tmpl(v: Option<&Value>) -> String { // = JS `${v}`
    match v { None => "undefined".into(), Some(Value::Null) => "null".into(), Some(x) => str_qq(Some(x)) }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn js_number(v: Option<&Value>) -> f64 { // = JS Number(v)
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }

fn strip_ord_suffix(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r" · \d+$").unwrap());
    re.replace(s, "").to_string()
}

// ── book-swarm's own module-private helpers (NAMES mirror the JS top-level consts/fns) ──────

/// `const out2obj = (aiResult) => {...}` — collapse `aiResult.output_kv[]` into `{field_name:
/// value_text}`, dropping entries whose `value_text` is nullish. Pure.
fn out2obj(ai_result: &Value) -> Value {
    let mut o = Map::new();
    if let Some(arr) = ai_result.get("output_kv").and_then(Value::as_array) {
        for f in arr {
            if let Some(vt) = f.get("value_text") {
                if !vt.is_null() {
                    if let Some(fname) = f.get("field_name").and_then(Value::as_str) {
                        o.insert(fname.to_string(), vt.clone());
                    }
                }
            }
        }
    }
    Value::Object(o)
}

/// `const parseArr = (x) => {...}` — JSON.parse a string, pass an array through, else `[]`. Pure.
fn parse_arr(x: &Value) -> Vec<Value> {
    match x {
        Value::String(s) => serde_json::from_str::<Value>(s).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default(),
        Value::Array(a) => a.clone(),
        _ => Vec::new(),
    }
}

/// `const costOf = (db, id) => {...}` — best-effort `brain.ai_calls.cost_usd_actual` lookup;
/// any failure (missing row, db error) collapses to 0.
fn cost_of(db: &Connection, id: &Value) -> f64 {
    if id.is_null() { return 0.0; }
    match crate::db_json::query_rows(db, "SELECT cost_usd_actual FROM brain.ai_calls WHERE id = ?", &[id.clone()]) {
        Ok(v) => v
            .as_array()
            .and_then(|a| a.first())
            .and_then(|r| r.get("cost_usd_actual"))
            .map(|x| js_number(Some(x)))
            .filter(|f| f.is_finite())
            .unwrap_or(0.0),
        Err(_) => 0.0,
    }
}

/// `const slug = (s) => {...}` — lowercase, non-alnum runs to spaces, trim, first 12 words joined
/// by `-`, capped at 160 chars. Pure.
fn slug(v: &Value) -> String {
    let s = str_qq(Some(v)).to_lowercase();
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"[^a-z0-9]+").unwrap());
    let replaced = re.replace_all(&s, " ");
    let words: Vec<&str> = replaced.trim().split_whitespace().take(12).collect();
    take_chars(&words.join("-"), 160)
}

/// `function sliceAround(blocks, ord, line, span = 14)` — a `[line-3, line+span)` window of the
/// matching block's text, cleaned to 1400 chars. Pure (over the given `blocks` snapshot).
fn slice_around(blocks: &Value, ord: &Value, line: i64, span: i64) -> String {
    let Some(b) = blocks.as_array().and_then(|a| a.iter().find(|x| x.get("ord").cloned().unwrap_or(Value::Null) == *ord)) else {
        return String::new();
    };
    let text = match b.get("text") { Some(v) => js_string(v), None => "undefined".to_string() };
    let lines: Vec<&str> = text.split('\n').collect();
    let len = lines.len() as i64;
    let start = (line - 3).max(0).min(len) as usize;
    let end = (line + span).max(0).min(len).max(start as i64) as usize;
    take_chars(&lines[start..end].join("\n"), 1400)
}

/// `const PROMPTS = {...}` — the six prompt templates registered on first use by `ensurePrompt`.
/// Returns (template_md, max_output_tokens, output_fields). The questioneer template bakes in
/// `INSTRUMENTS.join(' · ')` at compile time — the JS builds it via a template literal at MODULE
/// LOAD time (not a `{{}}` placeholder), so the joined string is inlined verbatim here.
fn prompt_def(name: &str) -> Option<(&'static str, i64, Value)> {
    match name {
        "book.definer.v1" => Some((
            "You are THE DEFINER — one lens, one job: define ONE term in the book's own sense.\nTERM: {{term}}\nTHE RECEIPT (the sentence/field the atlas found — your ONLY ground): {{receipt}}\nCONTEXT SLICE (same block, for disambiguation only): {{slice}}\nRules: <=45 words, the book's meaning (never your general knowledge), no examples, no hedging. If the receipt does not actually define this term, refuse.\nJSON only: {\"definition\": \"<=45 words\", \"refuse\": \"<why this slot is not a real definition — omit when defining>\"}",
            300,
            json!([{"field_name":"definition","field_type":"string","required":true},{"field_name":"refuse","field_type":"string","required":false}]),
        )),
        "book.lawwright.v1" => Some((
            "You are THE LAWWRIGHT — one lens, one job: press ONE normative sentence into a ratifiable rule.\nTHE NORMATIVE SENTENCE (the atlas's receipt — your ONLY ground): {{receipt}}\nCONTEXT SLICE: {{slice}}\nRules: statement = one enforceable rule in OUR engineering context; keep the book's OWN qualifications as exceptions (verbatim fragments); refuse advice too vague to enforce.\nJSON only: {\"key\": \"BOOK-<SLUG>-<NN>\", \"statement\": \"\", \"why\": \"one line\", \"exceptions\": [\"the book's own qualifiers\"], \"refuse\": \"<omit when pressing>\"}",
            420,
            json!([{"field_name":"key","field_type":"string","required":true},{"field_name":"statement","field_type":"string","required":true},{"field_name":"why","field_type":"string","required":false},{"field_name":"exceptions","field_type":"string","required":false},{"field_name":"refuse","field_type":"string","required":false}]),
        )),
        "book.questioneer.v1" => Some((
            "You are THE QUESTIONEER — one lens, one job: turn ONE of the book's own review questions into an interview prompt our review engine can conduct.\nTHE QUESTION (receipt): {{receipt}}\nOUR DETERMINISTIC INSTRUMENTS (the only evidence sources you may name): public API surface · import/call graph · blast radius · coupling axis · health axis · file size/LOC · test coverage · git churn · ownership/bus-factor\nJSON only: {\"question\": \"the cleaned reusable form\", \"evidence_required\": [\"instrument names from the list\"], \"answerable\": \"yes|no — no when none of our instruments can ground it\"}",
            260,
            json!([{"field_name":"question","field_type":"string","required":true},{"field_name":"evidence_required","field_type":"string","required":true},{"field_name":"answerable","field_type":"string","required":true}]),
        )),
        "book.tradeoff.v1" => Some((
            "You are THE WEIGHER — one lens, one job: split ONE consequences/trade-offs passage into claims.\nSUBJECT: {{term}}\nTHE PASSAGE (receipt slice): {{slice}}\nJSON only: {\"claims\": [{\"side\": \"benefit|cost\", \"text\": \"<=25 words, the book's claim\"}]} — 2..6 claims, the book's claims only.",
            420,
            json!([{"field_name":"claims","field_type":"string","required":true}]),
        )),
        "book.skillsmith.v1" => Some((
            "You are THE SKILLSMITH — one lens, one job: press ONE listing/procedure into a reusable method.\nLISTING HEAD: {{term}}\nTHE SURROUNDING SLICE: {{slice}}\nRules: body_md = steps + pitfalls, grounded on the slice; refuse boilerplate with no method in it.\nJSON only: {\"name\": \"\", \"description\": \"one when-to-use line\", \"body_md\": \"\", \"refuse\": \"<omit when pressing>\"}",
            700,
            json!([{"field_name":"name","field_type":"string","required":true},{"field_name":"description","field_type":"string","required":true},{"field_name":"body_md","field_type":"string","required":true},{"field_name":"refuse","field_type":"string","required":false}]),
        )),
        "book.warden.v1" => Some((
            "You are THE WARDEN — adversarial, one job: try to KILL this unit against its receipt.\nTHE UNIT ({{kind}}): {{draft}}\nTHE RECEIPT (the only ground truth): {{receipt}}\nKill when: it says more than the receipt supports, drifts from the book's sense, or is generic filler.\nJSON only: {\"verdict\": \"keep|kill\", \"reason\": \"one line\"}",
            160,
            json!([{"field_name":"verdict","field_type":"string","required":true},{"field_name":"reason","field_type":"string","required":true}]),
        )),
        "book.dialogue.v1" => Some((
            "You are THE COMPARER — two authorities wrote about the same thing; name the relation, never resolve it.\nA ({{from_authority}}): {{from_title}}\nB ({{to_authority}}): {{to_title}}\nJSON only: {\"stance\": \"SUPPORTS|CONFLICTS|REFINES\", \"topic\": \"<=8 words\"}",
            200,
            json!([{"field_name":"stance","field_type":"string","required":true},{"field_name":"topic","field_type":"string","required":true}]),
        )),
        _ => None,
    }
}

/// `function ensurePrompt(db, name)` — find-or-register. On find failure, register from
/// `prompt_def` (default_tier 'low', default_temperature 'cold') then re-find.
fn ensure_prompt(db: &Connection, name: &str) -> Result<Value, String> {
    if let Ok(v) = crate::ai_prompt_find::promptFind(&json!({ "name": name }), db) {
        return Ok(v);
    }
    let (md, max, fields) = prompt_def(name).ok_or_else(|| format!("book-swarm: unknown prompt '{name}'"))?;
    crate::ai_prompt_add::promptAdd(
        &json!({ "name": name, "template_md": md, "default_tier": "low", "default_temperature": "cold", "max_output_tokens": max, "output_fields": fields }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!({ "name": name }), db)
}

/// `const call = async (name, kv, intent) => {...}` — ensure the prompt is registered, dispatch
/// `aiCall` (the PARITY-DEFERRED boundary — see module doc), accumulate cost, shape the response.
fn call_ai(db: &Connection, name: &str, kv: Value, intent: &str, spent: &mut f64) -> Result<Value, String> {
    ensure_prompt(db, name)?;
    let r = crate::ai_ai_call::aiCall(&json!({ "prompt_name": name, "input_kv": kv, "provider_fn": Value::Null, "intent": intent }), db)?;
    let ai_call_id = r.get("ai_call_id").cloned().unwrap_or(Value::Null);
    *spent += cost_of(db, &ai_call_id);
    Ok(out2obj(&r))
}

fn put_unit(db: &Connection, unit: Value, authority: &str, book_id: i64) -> Result<Value, String> {
    let mut u = unit;
    if let Some(o) = u.as_object_mut() {
        o.insert("authority".into(), json!(authority));
        o.insert("book_id".into(), json!(book_id));
    }
    crate::entities_knowledge_ir::knowledgeIr(&json!({ "op": "put", "unit": u }), db)
}

fn skip_event(skips: &mut Map<String, Value>, events: &mut Vec<Value>, t: &str, why: &str) {
    let cur = skips.get(t).and_then(Value::as_i64).unwrap_or(0);
    skips.insert(t.to_string(), json!(cur + 1));
    events.push(json!({ "type": t, "skipped": take_chars(why, 100) }));
}

fn parse_json_obj(v: Option<&Value>) -> Map<String, Value> {
    let s = v.and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("{}");
    match serde_json::from_str::<Value>(s) { Ok(Value::Object(m)) => m, _ => Map::new() }
}

pub fn bookSwarm(input: &Value, db: &Connection) -> Result<Value, String> {
    let op = match input.get("op") { Some(v) if !v.is_null() => js_string(v), _ => "status".to_string() };

    if op == "start" {
        let book_id_num = js_number(input.get("book_id"));
        if !book_id_num.is_finite() || book_id_num.fract() != 0.0 || book_id_num <= 0.0 {
            return Err("book-swarm: book_id is required".into());
        }
        let book_id = book_id_num as i64;
        let book_rows = crate::db_json::query_rows(db, "SELECT id FROM knowledge.books WHERE id = ?", &[json!(book_id)])?;
        if book_rows.as_array().map_or(true, |a| a.is_empty()) {
            return Err(format!("book-swarm: book '{book_id}' not found"));
        }
        let budget_v = match nz(input.get("budget_usd")) { Some(v) => json!(js_number(Some(v))), None => Value::Null };
        let (_c, id) = crate::db_json::run_write(db, "INSERT INTO knowledge.book_swarm_runs (book_id, budget_usd) VALUES (?, ?)", &[json!(book_id), budget_v])?;
        let rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.book_swarm_runs WHERE id = ?", &[json!(id)])?;
        let run = rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
        return Ok(json!({ "ok": true, "run": run }));
    }

    if op == "pause" || op == "resume" {
        let run_id = js_number(input.get("run_id")) as i64;
        let rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.book_swarm_runs WHERE id = ?", &[json!(run_id)])?;
        let Some(run) = rows.as_array().and_then(|a| a.first()).cloned() else {
            return Err(format!("book-swarm: run '{run_id}' not found"));
        };
        let status = if op == "pause" { "paused" } else { "running" };
        crate::db_json::run_write(db, "UPDATE knowledge.book_swarm_runs SET status = ?, updated_at = datetime('now') WHERE id = ?", &[json!(status), json!(run_id)])?;
        let mut run_out = run;
        if let Some(o) = run_out.as_object_mut() { o.insert("status".into(), json!(status)); }
        return Ok(json!({ "ok": true, "run": run_out }));
    }

    if op == "status" {
        let mut run: Option<Value> = None;
        if !input.get("run_id").map_or(true, Value::is_null) {
            let run_id = js_number(input.get("run_id")) as i64;
            let rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.book_swarm_runs WHERE id = ?", &[json!(run_id)])?;
            run = rows.as_array().and_then(|a| a.first()).cloned();
        } else if !input.get("book_id").map_or(true, Value::is_null) {
            let bid = js_number(input.get("book_id")) as i64;
            let rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.book_swarm_runs WHERE book_id = ? ORDER BY id DESC LIMIT 1", &[json!(bid)])?;
            run = rows.as_array().and_then(|a| a.first()).cloned();
        }
        let book_id_v: Option<i64> = run
            .as_ref()
            .and_then(|r| r.get("book_id"))
            .and_then(Value::as_i64)
            .or_else(|| {
                let n = js_number(input.get("book_id"));
                if n.is_finite() && n.fract() == 0.0 { Some(n as i64) } else { None }
            });
        let counts = match book_id_v {
            Some(bid) => Some(crate::entities_knowledge_ir::knowledgeIr(&json!({ "op": "counts", "book_id": bid }), db)?),
            None => None,
        };
        let mut units: Value = Value::Null;
        if truthy(input.get("list_kind")) {
            if let Some(bid) = book_id_v {
                let raw_limit = nz(input.get("limit")).cloned().unwrap_or(json!(20));
                let n = js_number(Some(&raw_limit));
                let lim = if n != 0.0 && !n.is_nan() { n } else { 20.0 }.min(50.0);
                let list_res = crate::entities_knowledge_ir::knowledgeIr(
                    &json!({ "op": "list", "kind": js_string(input.get("list_kind").unwrap()), "book_id": bid, "limit": lim }),
                    db,
                )?;
                let mapped: Vec<Value> = list_res
                    .get("units")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|u| json!({ "id": u.get("id").cloned().unwrap_or(Value::Null), "title": u.get("title").cloned().unwrap_or(Value::Null), "body": u.get("body").cloned().unwrap_or(Value::Null), "receipts": u.get("receipts").cloned().unwrap_or(Value::Null), "verdict": u.get("verdict").cloned().unwrap_or(Value::Null) }))
                    .collect();
                units = json!(mapped);
            }
        }
        let skips = run.as_ref().map(|r| Value::Object(parse_json_obj(r.get("skips_json")))).unwrap_or(json!({}));
        return Ok(json!({
            "ok": true,
            "run": run.unwrap_or(Value::Null),
            "counts": counts.as_ref().and_then(|c| c.get("by_kind")).cloned().unwrap_or(json!({})),
            "edges": counts.as_ref().and_then(|c| c.get("edges")).cloned().unwrap_or(json!(0)),
            "skips": skips,
            "units": units,
        }));
    }

    if op != "step" {
        return Err("book-swarm: op 'start'|'step'|'pause'|'resume'|'status' is required".into());
    }

    // ── STEP: fill the next BATCH of slots, cost-break-gated. ──────────────────────────────
    let run_id = js_number(input.get("run_id")) as i64;
    let rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.book_swarm_runs WHERE id = ?", &[json!(run_id)])?;
    let Some(run) = rows.as_array().and_then(|a| a.first()).cloned() else {
        return Err(format!("book-swarm: run '{run_id}' not found"));
    };
    let run_status = run.get("status").and_then(Value::as_str).unwrap_or("");
    if run_status != "running" {
        return Ok(json!({ "ok": false, "error": format!("run is {run_status}"), "run": run }));
    }
    let book_id = run.get("book_id").and_then(Value::as_i64).unwrap_or(0);
    let book_rows = crate::db_json::query_rows(db, "SELECT id, title, author, created_at FROM knowledge.books WHERE id = ?", &[json!(book_id)])?;
    let book = book_rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(json!({}));
    let blocks_v = crate::db_json::query_rows(db, "SELECT ord, title, text FROM knowledge.book_blocks WHERE book_id = ? ORDER BY ord", &[json!(book_id)])?;

    let mut atlas = crate::agents_in_book_atlas::buildAtlas(&blocks_v);
    let anns = crate::agents_in_book_atlas::loadAnnotations(db, &json!(book_id));
    if let Some(slots_obj) = atlas.get_mut("slots").and_then(Value::as_object_mut) {
        for t in ["definitions", "normatives", "questions", "tradeoffs", "listings", "procedures", "glossary"] {
            if let Some(arr) = slots_obj.get(t).cloned() {
                if arr.is_array() {
                    let updated = crate::agents_in_book_atlas::applyAnnotations(&json!(t), &arr, &anns);
                    slots_obj.insert(t.to_string(), updated);
                }
            }
        }
    }
    let authority: String = take_chars(&js_string(book.get("title").unwrap_or(&Value::Null)), 200);

    let get_slot = |t: &str| -> Vec<Value> { atlas.get("slots").and_then(|s| s.get(t)).and_then(Value::as_array).cloned().unwrap_or_default() };
    let dialogue_res = crate::entities_knowledge_ir::knowledgeIr(&json!({ "op": "dialogue", "limit": 50 }), db)?;
    let dialogue_pairs: Vec<Value> = dialogue_res.get("pairs").and_then(Value::as_array).cloned().unwrap_or_default();

    let queues: Vec<(&str, Vec<Value>)> = vec![
        ("glossary", get_slot("glossary")),
        ("definitions", get_slot("definitions")),
        ("normatives", get_slot("normatives")),
        ("questions", get_slot("questions")),
        ("tradeoffs", get_slot("tradeoffs")),
        ("listings", get_slot("listings")),
        ("procedures", get_slot("procedures")),
        ("dialogue", dialogue_pairs),
    ];

    let mut cursors = parse_json_obj(run.get("cursors_json"));
    let mut skips = parse_json_obj(run.get("skips_json"));
    let mut spent = { let n = js_number(run.get("spent_usd")); if n.is_finite() { n } else { 0.0 } };
    let budget: Option<f64> = if nz(run.get("budget_usd")).is_some() { Some(js_number(run.get("budget_usd"))) } else { None };
    let batch: i64 = {
        let raw = nz(input.get("batch")).cloned().unwrap_or(json!(4));
        let n = js_number(Some(&raw));
        let n2 = if n != 0.0 && !n.is_nan() { n } else { 4.0 };
        n2.max(1.0).min(10.0) as i64
    };
    let only_type: Option<String> = match input.get("only") { Some(v) if !v.is_null() => Some(js_string(v)), _ => None };
    let mut events: Vec<Value> = Vec::new();
    let run_id_v = json!(run_id);

    let step_result: Result<Value, String> = (|| -> Result<Value, String> {
        let mut did_work: i64 = 0;
        for (t, list) in queues.iter() {
            if let Some(only) = &only_type { if only.as_str() != *t { continue; } }
            if did_work >= batch { break; }
            let mut i = cursors.get(*t).and_then(Value::as_i64).unwrap_or(0);
            while i < list.len() as i64 && did_work < batch {
                if let Some(bud) = budget {
                    if spent >= bud {
                        cursors.insert(t.to_string(), json!(i));
                        crate::db_json::run_write(
                            db,
                            "UPDATE knowledge.book_swarm_runs SET status='cost_break', spent_usd=?, cursors_json=?, skips_json=?, updated_at=datetime('now') WHERE id=?",
                            &[json!(spent), json!(Value::Object(cursors.clone()).to_string()), json!(Value::Object(skips.clone()).to_string()), run_id_v.clone()],
                        )?;
                        let mut run_out = run.clone();
                        if let Some(o) = run_out.as_object_mut() { o.insert("status".into(), json!("cost_break")); o.insert("spent_usd".into(), json!(spent)); }
                        return Ok(json!({ "ok": true, "run": run_out, "events": events, "cost_break": true }));
                    }
                }
                let slot = list[i as usize].clone();
                i += 1;
                cursors.insert(t.to_string(), json!(i));
                did_work += 1;

                match *t {
                    "glossary" => {
                        let key = str_qq(slot.get("term")).to_lowercase();
                        let existing = crate::entities_knowledge_ir::knowledgeIr(&json!({ "op": "list", "kind": "concept", "identity_key": key, "book_id": book_id, "limit": 1 }), db)?;
                        if !existing.get("units").and_then(Value::as_array).map_or(false, |a| !a.is_empty()) {
                            let unit = json!({
                                "kind": "concept", "identity_key": key, "title": slot.get("term").cloned().unwrap_or(Value::Null),
                                "body": { "definition": slot.get("definition").cloned().unwrap_or(Value::Null) },
                                "receipts": [slot.get("receipt").cloned().unwrap_or(Value::Null)], "type_class": "definition",
                                "consumers": ["rag", "ontology", "paper", "glossary"]
                            });
                            put_unit(db, unit, &authority, book_id)?;
                            events.push(json!({ "type": t, "kept": slot.get("term").cloned().unwrap_or(Value::Null) }));
                        }
                        did_work -= 1; // free work never eats the batch
                    }
                    "procedures" => {
                        let steps = slot.get("steps").cloned().unwrap_or(Value::Null);
                        let block_ord = slot.get("block_ord").cloned().unwrap_or(Value::Null);
                        let line = slot.get("line").and_then(Value::as_i64).unwrap_or(0);
                        let slice = slice_around(&blocks_v, &block_ord, line, 26);
                        let o = call_ai(db, "book.skillsmith.v1", json!([{ "key": "term", "value": format!("a {}-step procedure", tmpl(Some(&steps))) }, { "key": "slice", "value": slice }]), "system.book.swarm", &mut spent)?;
                        let name = o.get("name").cloned().unwrap_or(Value::Null);
                        let refuse = o.get("refuse").cloned().unwrap_or(Value::Null);
                        if !truthy(Some(&name)) || truthy(Some(&refuse)) {
                            skip_event(&mut skips, &mut events, t, &if truthy(Some(&refuse)) { js_string(&refuse) } else { "no method".into() });
                            continue;
                        }
                        let receipt = slot.get("receipt").cloned().unwrap_or(Value::Null);
                        let quote = receipt.get("quote").cloned().unwrap_or(Value::Null);
                        let draft = format!("{}: {}", js_string(&name), js_string(&o.get("description").cloned().unwrap_or(Value::Null)));
                        let w = call_ai(db, "book.warden.v1", json!([{ "key": "kind", "value": "method" }, { "key": "draft", "value": draft }, { "key": "receipt", "value": quote }]), "system.book.swarm", &mut spent)?;
                        if str_qq(w.get("verdict")).to_lowercase() != "keep" {
                            skip_event(&mut skips, &mut events, t, &format!("warden: {}", str_qq(w.get("reason"))));
                            continue;
                        }
                        let unit = json!({
                            "kind": "method", "identity_key": slug(&name), "title": take_chars(&js_string(&name), 120),
                            "body": { "description": o.get("description").cloned().unwrap_or(Value::Null), "body_md": take_chars(&js_string(&o.get("body_md").cloned().unwrap_or(Value::Null)), 6000), "steps": steps },
                            "receipts": [receipt], "type_class": "example", "consumers": ["skilllib", "paper"]
                        });
                        put_unit(db, unit, &authority, book_id)?;
                        events.push(json!({ "type": t, "kept": name }));
                    }
                    "definitions" => {
                        let key = str_qq(slot.get("term")).to_lowercase();
                        let existing = crate::entities_knowledge_ir::knowledgeIr(&json!({ "op": "list", "kind": "concept", "identity_key": key, "book_id": book_id, "limit": 1 }), db)?;
                        if existing.get("units").and_then(Value::as_array).map_or(false, |a| !a.is_empty()) {
                            did_work -= 1;
                            continue;
                        }
                        let receipt = slot.get("receipt").cloned().unwrap_or(Value::Null);
                        let quote = receipt.get("quote").cloned().unwrap_or(Value::Null);
                        let r_block_ord = receipt.get("block_ord").cloned().unwrap_or(Value::Null);
                        let r_line = receipt.get("line").and_then(Value::as_i64).unwrap_or(0);
                        let slice = slice_around(&blocks_v, &r_block_ord, r_line, 14);
                        let o = call_ai(db, "book.definer.v1", json!([{ "key": "term", "value": slot.get("term").cloned().unwrap_or(Value::Null) }, { "key": "receipt", "value": quote.clone() }, { "key": "slice", "value": slice }]), "system.book.swarm", &mut spent)?;
                        let definition = o.get("definition").cloned().unwrap_or(Value::Null);
                        let refuse = o.get("refuse").cloned().unwrap_or(Value::Null);
                        if !truthy(Some(&definition)) || truthy(Some(&refuse)) {
                            skip_event(&mut skips, &mut events, t, &if truthy(Some(&refuse)) { js_string(&refuse) } else { "no definition".into() });
                            continue;
                        }
                        let draft = format!("{}: {}", str_qq(slot.get("term")), js_string(&definition));
                        let w = call_ai(db, "book.warden.v1", json!([{ "key": "kind", "value": "concept" }, { "key": "draft", "value": draft }, { "key": "receipt", "value": quote }]), "system.book.swarm", &mut spent)?;
                        if str_qq(w.get("verdict")).to_lowercase() != "keep" {
                            skip_event(&mut skips, &mut events, t, &format!("warden: {}", str_qq(w.get("reason"))));
                            continue;
                        }
                        let unit = json!({
                            "kind": "concept", "identity_key": key, "title": slot.get("term").cloned().unwrap_or(Value::Null),
                            "body": { "definition": definition }, "receipts": [receipt], "type_class": "definition",
                            "consumers": ["rag", "ontology", "paper", "glossary"]
                        });
                        put_unit(db, unit, &authority, book_id)?;
                        events.push(json!({ "type": t, "kept": slot.get("term").cloned().unwrap_or(Value::Null) }));
                    }
                    "normatives" => {
                        let receipt = slot.get("receipt").cloned().unwrap_or(Value::Null);
                        let quote = receipt.get("quote").cloned().unwrap_or(Value::Null);
                        let block_ord = slot.get("block_ord").cloned().unwrap_or(Value::Null);
                        let r_line = receipt.get("line").and_then(Value::as_i64).unwrap_or(0);
                        let slice = slice_around(&blocks_v, &block_ord, r_line, 14);
                        let o = call_ai(db, "book.lawwright.v1", json!([{ "key": "receipt", "value": quote.clone() }, { "key": "slice", "value": slice }]), "system.book.swarm", &mut spent)?;
                        let statement = o.get("statement").cloned().unwrap_or(Value::Null);
                        let refuse = o.get("refuse").cloned().unwrap_or(Value::Null);
                        if !truthy(Some(&statement)) || truthy(Some(&refuse)) {
                            skip_event(&mut skips, &mut events, t, &if truthy(Some(&refuse)) { js_string(&refuse) } else { "unenforceable".into() });
                            continue;
                        }
                        let w = call_ai(db, "book.warden.v1", json!([{ "key": "kind", "value": "doctrine" }, { "key": "draft", "value": statement.clone() }, { "key": "receipt", "value": quote }]), "system.book.swarm", &mut spent)?;
                        if str_qq(w.get("verdict")).to_lowercase() != "keep" {
                            skip_event(&mut skips, &mut events, t, &format!("warden: {}", str_qq(w.get("reason"))));
                            continue;
                        }
                        let key_field = o.get("key").cloned().unwrap_or(Value::Null);
                        let identity_src = if truthy(Some(&key_field)) { key_field.clone() } else { statement.clone() };
                        let exceptions: Vec<Value> = parse_arr(&o.get("exceptions").cloned().unwrap_or(Value::Null)).into_iter().map(|v| json!(js_string(&v))).collect();
                        let unit = json!({
                            "kind": "doctrine", "identity_key": slug(&identity_src), "title": take_chars(&js_string(&identity_src), 120),
                            "body": { "statement": statement.clone(), "why": o.get("why").cloned().unwrap_or(json!("")) },
                            "receipts": [receipt], "type_class": "normative", "exceptions": exceptions,
                            "consumers": ["law-draft", "paper", "ultra-review"]
                        });
                        put_unit(db, unit, &authority, book_id)?;
                        let kept_val = if nz(Some(&key_field)).is_some() { key_field } else { json!(take_chars(&js_string(&statement), 40)) };
                        events.push(json!({ "type": t, "kept": kept_val }));
                    }
                    "questions" => {
                        let receipt = slot.get("receipt").cloned().unwrap_or(Value::Null);
                        let quote = receipt.get("quote").cloned().unwrap_or(Value::Null);
                        let o = call_ai(db, "book.questioneer.v1", json!([{ "key": "receipt", "value": quote }]), "system.book.swarm", &mut spent)?;
                        let question = o.get("question").cloned().unwrap_or(Value::Null);
                        if !truthy(Some(&question)) {
                            skip_event(&mut skips, &mut events, t, "no question");
                            continue;
                        }
                        let evidence_required: Vec<Value> = parse_arr(&o.get("evidence_required").cloned().unwrap_or(Value::Null)).into_iter().map(|v| json!(js_string(&v))).collect();
                        let answerable = str_qq(o.get("answerable")).to_lowercase().contains("yes");
                        let unit = json!({
                            "kind": "question", "identity_key": slug(&question), "title": take_chars(&js_string(&question), 200),
                            "body": { "question": question.clone(), "evidence_required": evidence_required, "answerable": answerable },
                            "receipts": [receipt], "type_class": "normative", "consumers": ["ultra-review", "paper"]
                        });
                        put_unit(db, unit, &authority, book_id)?;
                        events.push(json!({ "type": t, "kept": take_chars(&js_string(&question), 40) }));
                    }
                    "tradeoffs" => {
                        let block_ord = slot.get("block_ord").cloned().unwrap_or(Value::Null);
                        let line = slot.get("line").and_then(Value::as_i64).unwrap_or(0);
                        let label = str_qq(slot.get("label"));
                        let b = blocks_v.as_array().and_then(|a| a.iter().find(|x| x.get("ord").cloned().unwrap_or(Value::Null) == block_ord));
                        let raw_title = b.and_then(|x| x.get("title")).cloned().unwrap_or(json!("subject"));
                        let term = strip_ord_suffix(&js_string(&raw_title));
                        let slice = slice_around(&blocks_v, &block_ord, line, 24);
                        let o = call_ai(db, "book.tradeoff.v1", json!([{ "key": "term", "value": term.clone() }, { "key": "slice", "value": slice }]), "system.book.swarm", &mut spent)?;
                        let claims: Vec<Value> = parse_arr(&o.get("claims").cloned().unwrap_or(Value::Null)).into_iter().filter(|c| truthy(c.get("text"))).take(6).collect();
                        if claims.is_empty() {
                            skip_event(&mut skips, &mut events, t, "no claims");
                            continue;
                        }
                        let identity_key = format!("{}-{}-{}", slug(&json!(term)), label.to_lowercase(), tmpl(Some(&block_ord)));
                        let unit = json!({
                            "kind": "trade-off", "identity_key": identity_key, "title": format!("{term} — {label}"),
                            "body": { "claims": claims }, "receipts": [{ "block_ord": block_ord, "line": line, "quote": format!("{label} of {term}") }],
                            "type_class": "normative", "consumers": ["paper", "ultra-review"]
                        });
                        put_unit(db, unit, &authority, book_id)?;
                        events.push(json!({ "type": t, "kept": term }));
                    }
                    "listings" => {
                        let head = slot.get("head").cloned().unwrap_or(Value::Null);
                        let block_ord = slot.get("block_ord").cloned().unwrap_or(Value::Null);
                        let line = slot.get("line").and_then(Value::as_i64).unwrap_or(0);
                        let slice = slice_around(&blocks_v, &block_ord, line, 30);
                        let o = call_ai(db, "book.skillsmith.v1", json!([{ "key": "term", "value": head.clone() }, { "key": "slice", "value": slice }]), "system.book.swarm", &mut spent)?;
                        let name = o.get("name").cloned().unwrap_or(Value::Null);
                        let refuse = o.get("refuse").cloned().unwrap_or(Value::Null);
                        if !truthy(Some(&name)) || truthy(Some(&refuse)) {
                            skip_event(&mut skips, &mut events, t, &if truthy(Some(&refuse)) { js_string(&refuse) } else { "no method".into() });
                            continue;
                        }
                        let draft = format!("{}: {}", js_string(&name), js_string(&o.get("description").cloned().unwrap_or(Value::Null)));
                        let w = call_ai(db, "book.warden.v1", json!([{ "key": "kind", "value": "method" }, { "key": "draft", "value": draft }, { "key": "receipt", "value": head.clone() }]), "system.book.swarm", &mut spent)?;
                        if str_qq(w.get("verdict")).to_lowercase() != "keep" {
                            skip_event(&mut skips, &mut events, t, &format!("warden: {}", str_qq(w.get("reason"))));
                            continue;
                        }
                        let unit = json!({
                            "kind": "method", "identity_key": slug(&name), "title": take_chars(&js_string(&name), 120),
                            "body": { "description": o.get("description").cloned().unwrap_or(Value::Null), "body_md": take_chars(&js_string(&o.get("body_md").cloned().unwrap_or(Value::Null)), 6000) },
                            "receipts": [{ "block_ord": block_ord, "line": line, "quote": head }], "type_class": "example", "consumers": ["skilllib", "paper"]
                        });
                        put_unit(db, unit, &authority, book_id)?;
                        events.push(json!({ "type": t, "kept": name }));
                    }
                    "dialogue" => {
                        let o = call_ai(
                            db,
                            "book.dialogue.v1",
                            json!([
                                { "key": "from_authority", "value": slot.get("from_authority").cloned().unwrap_or(Value::Null) },
                                { "key": "from_title", "value": slot.get("from_title").cloned().unwrap_or(Value::Null) },
                                { "key": "to_authority", "value": slot.get("to_authority").cloned().unwrap_or(Value::Null) },
                                { "key": "to_title", "value": slot.get("to_title").cloned().unwrap_or(Value::Null) },
                            ]),
                            "system.book.swarm",
                            &mut spent,
                        )?;
                        let stance_up = str_qq(o.get("stance")).to_uppercase();
                        let stance = if ["SUPPORTS", "CONFLICTS", "REFINES"].contains(&stance_up.as_str()) { stance_up } else { "RELATED".to_string() };
                        let topic = if truthy(o.get("topic")) { o.get("topic").cloned().unwrap() } else { slot.get("identity_key").cloned().unwrap_or(Value::Null) };
                        crate::entities_knowledge_ir::knowledgeIr(&json!({ "op": "edge", "from_unit": slot.get("from_unit").cloned().unwrap_or(Value::Null), "to_unit": slot.get("to_unit").cloned().unwrap_or(Value::Null), "kind": stance.clone(), "topic": topic }), db)?;
                        events.push(json!({ "type": t, "kept": format!("{}: {}", stance, str_qq(slot.get("identity_key"))) }));
                    }
                    _ => {}
                }
            }
        }

        // P4 — deterministic RELATED edges from the atlas's xrefs between IR concepts ($0, idempotent).
        let concepts_res = crate::entities_knowledge_ir::knowledgeIr(&json!({ "op": "list", "kind": "concept", "book_id": book_id, "limit": 500 }), db)?;
        let concepts = concepts_res.get("units").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut by_key: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
        for u in &concepts {
            if let Some(k) = u.get("identity_key").and_then(Value::as_str) {
                by_key.insert(k.to_string(), u.get("id").cloned().unwrap_or(Value::Null));
            }
        }
        let xref_edges = atlas.get("slots").and_then(|s| s.get("xrefs")).and_then(|x| x.get("edges")).and_then(Value::as_array).cloned().unwrap_or_default();
        let definitions_slot = atlas.get("slots").and_then(|s| s.get("definitions")).and_then(Value::as_array).cloned().unwrap_or_default();
        for e in &xref_edges {
            let term_key = str_qq(e.get("term")).to_lowercase();
            let to = by_key.get(&term_key).cloned();
            let e_block_ord = e.get("block_ord").cloned().unwrap_or(Value::Null);
            let from_block_def = definitions_slot.iter().find(|dd| dd.get("block_ord").cloned().unwrap_or(Value::Null) == e_block_ord);
            let from = from_block_def.and_then(|dd| by_key.get(&str_qq(dd.get("term")).to_lowercase()).cloned());
            if let (Some(f), Some(tt)) = (from, to) {
                if f != tt {
                    crate::entities_knowledge_ir::knowledgeIr(&json!({ "op": "edge", "from_unit": f, "to_unit": tt, "kind": "RELATED", "topic": "xref" }), db)?;
                }
            }
        }

        let total: i64 = queues.iter().map(|(_, l)| l.len() as i64).sum();
        let done_all = queues.iter().all(|(tt, l)| cursors.get(*tt).and_then(Value::as_i64).unwrap_or(0) >= l.len() as i64);
        let status = if done_all { "done" } else { "running" };
        crate::db_json::run_write(
            db,
            "UPDATE knowledge.book_swarm_runs SET status=?, spent_usd=?, cursors_json=?, skips_json=?, updated_at=datetime('now') WHERE id=?",
            &[json!(status), json!(spent), json!(Value::Object(cursors.clone()).to_string()), json!(Value::Object(skips.clone()).to_string()), run_id_v.clone()],
        )?;
        let counts = crate::entities_knowledge_ir::knowledgeIr(&json!({ "op": "counts", "book_id": book_id }), db)?;
        let mut run_out = run.clone();
        if let Some(o) = run_out.as_object_mut() {
            o.insert("status".into(), json!(status));
            o.insert("spent_usd".into(), json!(spent));
            o.insert("cursors_json".into(), json!(Value::Object(cursors.clone()).to_string()));
        }
        Ok(json!({ "ok": true, "run": run_out, "events": events, "counts": counts.get("by_kind").cloned().unwrap_or(json!({})), "edges": counts.get("edges").cloned().unwrap_or(json!(0)), "plan_total": total }))
    })();

    match step_result {
        Ok(v) => Ok(v),
        Err(e) => {
            let msg = take_chars(&e, 300);
            let _ = crate::db_json::run_write(
                db,
                "UPDATE knowledge.book_swarm_runs SET status='error', error=?, spent_usd=?, cursors_json=?, skips_json=?, updated_at=datetime('now') WHERE id=?",
                &[json!(msg.clone()), json!(spent), json!(Value::Object(cursors.clone()).to_string()), json!(Value::Object(skips.clone()).to_string()), run_id_v.clone()],
            );
            Ok(json!({ "ok": false, "error": msg, "events": events }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS knowledge; ATTACH ':memory:' AS brain;
            CREATE TABLE knowledge.books (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, author TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
            CREATE TABLE knowledge.book_blocks (id INTEGER PRIMARY KEY AUTOINCREMENT, book_id INTEGER NOT NULL, ord INTEGER NOT NULL, title TEXT, text TEXT NOT NULL);
            CREATE TABLE knowledge.book_annotations (id INTEGER PRIMARY KEY AUTOINCREMENT, book_id INTEGER NOT NULL, tool TEXT NOT NULL, slot_key TEXT NOT NULL, status TEXT NOT NULL, block_ord INTEGER, line INTEGER, payload_json TEXT);
            CREATE TABLE knowledge.book_swarm_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, book_id INTEGER NOT NULL, status TEXT NOT NULL DEFAULT 'running', budget_usd REAL, spent_usd REAL NOT NULL DEFAULT 0, cursors_json TEXT NOT NULL DEFAULT '{}', skips_json TEXT NOT NULL DEFAULT '{}', error TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), updated_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
            CREATE TABLE knowledge.knowledge_units (id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, identity_key TEXT NOT NULL, title TEXT NOT NULL, body_json TEXT NOT NULL DEFAULT '{}', receipts_json TEXT NOT NULL DEFAULT '[]', type_class TEXT NOT NULL DEFAULT 'definition', authority TEXT NOT NULL, edition TEXT, year INTEGER, applies_to TEXT NOT NULL DEFAULT '[]', exceptions_json TEXT NOT NULL DEFAULT '[]', consumers_json TEXT NOT NULL, book_id INTEGER, verdict TEXT NOT NULL DEFAULT 'kept', created_at TEXT, UNIQUE (kind, identity_key, authority));
            CREATE TABLE knowledge.unit_edges (id INTEGER PRIMARY KEY AUTOINCREMENT, from_unit INTEGER NOT NULL, to_unit INTEGER NOT NULL, kind TEXT NOT NULL, topic TEXT, receipts_json TEXT NOT NULL DEFAULT '[]', created_at TEXT, UNIQUE (from_unit, to_unit, kind));
            CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
            CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
            CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
            CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, cost_usd_actual REAL);",
        )
        .unwrap();
        c
    }

    // ── pure helpers ─────────────────────────────────────────────────────
    #[test]
    fn slug_basics() {
        assert_eq!(slug(&json!("The Observer Pattern!!")), "the-observer-pattern");
        assert_eq!(slug(&Value::Null), "");
        assert_eq!(slug(&json!("  multi   space   ")), "multi-space");
    }

    #[test]
    fn parse_arr_branches() {
        assert_eq!(parse_arr(&json!("[1,2,3]")), vec![json!(1), json!(2), json!(3)]);
        assert_eq!(parse_arr(&json!("not json")), Vec::<Value>::new());
        assert_eq!(parse_arr(&json!([1, 2])), vec![json!(1), json!(2)]);
        assert_eq!(parse_arr(&json!({"a": 1})), Vec::<Value>::new());
        assert_eq!(parse_arr(&Value::Null), Vec::<Value>::new());
    }

    #[test]
    fn out2obj_drops_nullish_value_text() {
        let ai_result = json!({ "output_kv": [
            { "field_name": "definition", "value_text": "means X" },
            { "field_name": "refuse", "value_text": null },
        ] });
        let o = out2obj(&ai_result);
        assert_eq!(o, json!({ "definition": "means X" }));
        assert_eq!(out2obj(&json!({})), json!({}));
    }

    #[test]
    fn slice_around_windows_and_caps() {
        let blocks = json!([{ "ord": 0, "title": "T", "text": (0..30).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n") }]);
        let s = slice_around(&blocks, &json!(0), 10, 14);
        assert!(s.starts_with("line7"));
        assert!(s.contains("line23"));
        assert_eq!(slice_around(&blocks, &json!(99), 0, 14), "");
    }

    #[test]
    fn cost_of_missing_row_is_zero() {
        let c = db();
        assert_eq!(cost_of(&c, &Value::Null), 0.0);
        assert_eq!(cost_of(&c, &json!(999)), 0.0);
        c.execute("INSERT INTO brain.ai_calls (id, cost_usd_actual) VALUES (1, 0.42)", []).unwrap();
        assert_eq!(cost_of(&c, &json!(1)), 0.42);
    }

    // ── ensurePrompt find-or-register ───────────────────────────────────
    #[test]
    fn ensure_prompt_registers_then_finds() {
        let c = db();
        let r1 = ensure_prompt(&c, "book.definer.v1").unwrap();
        assert_eq!(r1["name"], json!("book.definer.v1"));
        assert_eq!(r1["default_tier"], json!("low"));
        assert_eq!(r1["default_temperature"], json!("cold"));
        // second call finds the already-registered row (no duplicate version bump)
        let r2 = ensure_prompt(&c, "book.definer.v1").unwrap();
        assert_eq!(r2["version"], r1["version"]);
    }

    // ── op guards ────────────────────────────────────────────────────────
    #[test]
    fn start_requires_positive_integer_book_id_and_existing_book() {
        let c = db();
        assert!(bookSwarm(&json!({ "op": "start" }), &c).unwrap_err().contains("book_id is required"));
        assert!(bookSwarm(&json!({ "op": "start", "book_id": 0 }), &c).unwrap_err().contains("book_id is required"));
        assert!(bookSwarm(&json!({ "op": "start", "book_id": 999 }), &c).unwrap_err().contains("book '999' not found"));
    }

    #[test]
    fn unknown_op_is_required_error() {
        let c = db();
        assert!(bookSwarm(&json!({ "op": "bogus" }), &c).unwrap_err().contains("op 'start'|'step'|'pause'|'resume'|'status' is required"));
    }

    #[test]
    fn start_pause_resume_status_round_trip() {
        let c = db();
        c.execute("INSERT INTO knowledge.books (id, title) VALUES (1, 'GoF')", []).unwrap();
        let started = bookSwarm(&json!({ "op": "start", "book_id": 1, "budget_usd": 5.0 }), &c).unwrap();
        assert_eq!(started["run"]["status"], json!("running"));
        assert_eq!(started["run"]["budget_usd"], json!(5.0));
        let run_id = started["run"]["id"].as_i64().unwrap();

        let paused = bookSwarm(&json!({ "op": "pause", "run_id": run_id }), &c).unwrap();
        assert_eq!(paused["run"]["status"], json!("paused"));

        let resumed = bookSwarm(&json!({ "op": "resume", "run_id": run_id }), &c).unwrap();
        assert_eq!(resumed["run"]["status"], json!("running"));

        let status = bookSwarm(&json!({ "op": "status", "run_id": run_id }), &c).unwrap();
        assert_eq!(status["run"]["id"], json!(run_id));
        assert_eq!(status["counts"], json!({}));
        assert_eq!(status["edges"], json!(0));

        assert!(bookSwarm(&json!({ "op": "pause", "run_id": 99999 }), &c).unwrap_err().contains("run '99999' not found"));
    }

    // ── step: deterministic paths only (aiCall is the deferred boundary) ──
    #[test]
    fn step_with_no_slots_completes_to_done_without_any_ai_call() {
        let c = db();
        c.execute("INSERT INTO knowledge.books (id, title) VALUES (1, 'Empty Book')", []).unwrap();
        c.execute("INSERT INTO knowledge.book_blocks (book_id, ord, title, text) VALUES (1, 0, 'Ch1', 'nothing receiptable here at all')", []).unwrap();
        let started = bookSwarm(&json!({ "op": "start", "book_id": 1 }), &c).unwrap();
        let run_id = started["run"]["id"].as_i64().unwrap();
        let r = bookSwarm(&json!({ "op": "step", "run_id": run_id }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["run"]["status"], json!("done"));
        assert_eq!(r["events"], json!([]));
    }

    #[test]
    fn step_glossary_free_lunch_writes_ir_unit_with_no_ai_call() {
        let c = db();
        c.execute("INSERT INTO knowledge.books (id, title) VALUES (1, 'GoF')", []).unwrap();
        // back-matter Glossary block with a "Term — definition" line, matched by book-atlas's
        // glossaryCandidates (structure zone 'back', title containing 'Glossar').
        c.execute(
            "INSERT INTO knowledge.book_blocks (book_id, ord, title, text) VALUES (1, 0, 'Glossary', 'Adapter — converts the interface of a class into another interface clients expect.')",
            [],
        )
        .unwrap();
        let started = bookSwarm(&json!({ "op": "start", "book_id": 1 }), &c).unwrap();
        let run_id = started["run"]["id"].as_i64().unwrap();
        let r = bookSwarm(&json!({ "op": "step", "run_id": run_id }), &c).unwrap();
        assert_eq!(r["ok"], json!(true), "{r:?}");
        let kept: Vec<&str> = r["events"].as_array().unwrap().iter().filter_map(|e| e["kept"].as_str()).collect();
        assert!(kept.contains(&"Adapter"), "{r:?}");
        let units = crate::db_json::query_rows(&c, "SELECT * FROM knowledge.knowledge_units WHERE kind='concept'", &[]).unwrap();
        assert_eq!(units.as_array().unwrap().len(), 1);
        assert_eq!(units[0]["identity_key"], json!("adapter"));
        assert_eq!(units[0]["verdict"], json!("kept"));
    }

    #[test]
    fn step_reaching_a_definitions_slot_hits_the_deferred_ai_boundary_and_marks_run_error() {
        let c = db();
        c.execute("INSERT INTO knowledge.books (id, title) VALUES (1, 'GoF')", []).unwrap();
        // a definer-shaped sentence book-atlas's definitionCandidates regex will match.
        c.execute(
            "INSERT INTO knowledge.book_blocks (book_id, ord, title, text) VALUES (1, 0, 'Observer · 1', 'The Observer pattern is a way to notify many dependent objects automatically.')",
            [],
        )
        .unwrap();
        let started = bookSwarm(&json!({ "op": "start", "book_id": 1 }), &c).unwrap();
        let run_id = started["run"]["id"].as_i64().unwrap();
        let r = bookSwarm(&json!({ "op": "step", "run_id": run_id, "only": "definitions" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false), "{r:?}");
        assert!(r["error"].as_str().unwrap().contains("ai-call"), "{r:?}");
        let row = crate::db_json::query_rows(&c, "SELECT status, error FROM knowledge.book_swarm_runs WHERE id = ?", &[json!(run_id)]).unwrap();
        assert_eq!(row[0]["status"], json!("error"));
        assert!(row[0]["error"].as_str().unwrap().contains("ai-call"));
    }

    #[test]
    fn step_on_a_non_running_run_returns_ok_false_without_mutating_status() {
        let c = db();
        c.execute("INSERT INTO knowledge.books (id, title) VALUES (1, 'GoF')", []).unwrap();
        let started = bookSwarm(&json!({ "op": "start", "book_id": 1 }), &c).unwrap();
        let run_id = started["run"]["id"].as_i64().unwrap();
        bookSwarm(&json!({ "op": "pause", "run_id": run_id }), &c).unwrap();
        let r = bookSwarm(&json!({ "op": "step", "run_id": run_id }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("run is paused"));
    }

    #[test]
    fn cost_break_persists_status_and_returns_without_reaching_ai() {
        let c = db();
        c.execute("INSERT INTO knowledge.books (id, title) VALUES (1, 'GoF')", []).unwrap();
        c.execute(
            "INSERT INTO knowledge.book_blocks (book_id, ord, title, text) VALUES (1, 0, 'Observer · 1', 'The Observer pattern is a way to notify many dependent objects automatically.')",
            [],
        )
        .unwrap();
        // budget already exhausted at spent_usd 0 boundary: budget_usd=0, spent_usd default 0 -> spent>=budget true immediately
        let started = bookSwarm(&json!({ "op": "start", "book_id": 1, "budget_usd": 0 }), &c).unwrap();
        let run_id = started["run"]["id"].as_i64().unwrap();
        let r = bookSwarm(&json!({ "op": "step", "run_id": run_id, "only": "definitions" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true), "{r:?}");
        assert_eq!(r["cost_break"], json!(true));
        assert_eq!(r["run"]["status"], json!("cost_break"));
        let row = crate::db_json::query_rows(&c, "SELECT status FROM knowledge.book_swarm_runs WHERE id = ?", &[json!(run_id)]).unwrap();
        assert_eq!(row[0]["status"], json!("cost_break"));
    }
}
