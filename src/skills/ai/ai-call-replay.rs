//! ai-call-replay.rs — twin of skills/ai/ai-call-replay.mjs (re-run a stored AI call from its
//! persisted inputs). NAMES ARE CONTRACT: `loadOriginalInputs`, `getPromptName`, `diffOutputs`,
//! default `aiCallReplay`.
//!
//! PARITY-DEFERRED: the module's whole purpose is to re-dispatch the original prompt through
//! `crate::ai_ai_call::aiCall` (bypass_cache=true) — itself the documented always-`Err` stub (see
//! ai-call.rs's own module doc: a real network call to a BYOK LLM provider, non-deterministic,
//! unfixturable in principle). `aiCallReplay` here ports every DETERMINISTIC step faithfully —
//! guard validation, loading the original persisted call + its `ai_call_inputs` rows (following
//! `cache_source_call_id` when the original was itself a cache hit), resolving the prompt
//! name/version, reconstructing `input_kv`, and assembling the `aiCall` request payload — then
//! calls `crate::ai_ai_call::aiCall(&ai_input, db)?`, which always returns `Err`, so the tail of
//! the JS function (tagging `replay_of_id`, reading back `ai_call_outputs` for both calls, diffing
//! them, rendering the markdown summary) is unreachable here. That tail's pure comparison logic is
//! still ported as its own real, fixture-tested unit: `diffOutputs` (same convention as
//! ai-call.rs's 9 pure helpers alongside its own deferred default export).
//!
//! `loadOriginalInputs`/`getPromptName` are ported as Option-returning db-read helpers (JS returns
//! `null` on no-row) — real, testable, exercised directly (not just via `aiCallReplay`'s error
//! paths) so the SELECT/JOIN-by-`cache_source_call_id` logic has its own coverage independent of
//! the deferred tail.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand: nz(a).or(nz(b))

const GET_CALL_SQL: &str = "SELECT id, cached, cache_source_call_id, prompt_template_id, tier, temperature FROM brain.ai_calls WHERE id = ?";
const GET_INPUTS_SQL: &str = "SELECT key, value_text, order_index FROM brain.ai_call_inputs WHERE ai_call_id = ? ORDER BY order_index ASC, id ASC";

/// `function loadOriginalInputs(db, ai_call_id)` — follows `cache_source_call_id` (when the
/// original row was itself a cache hit) to find the persisted inputs. `None` when the call row
/// doesn't exist (JS `null`).
pub fn loadOriginalInputs(db: &Connection, ai_call_id: &Value) -> Option<Value> {
    let rows = crate::db_json::query_rows(db, GET_CALL_SQL, &[ai_call_id.clone()]).ok()?;
    let call = rows.as_array().and_then(|a| a.first()).cloned()?;
    let cached = truthy(call.get("cached"));
    let cache_source = nz(call.get("cache_source_call_id"));
    let source_id = if cached && truthy(cache_source) {
        cache_source.unwrap().clone()
    } else {
        ai_call_id.clone()
    };
    let inputs = crate::db_json::query_rows(db, GET_INPUTS_SQL, &[source_id.clone()]).unwrap_or(json!([]));
    let mut out = Map::new();
    out.insert("call".into(), call);
    out.insert("source_id".into(), source_id);
    out.insert("inputs".into(), inputs);
    Some(Value::Object(out))
}

/// `function getPromptName(db, prompt_template_id)` — `None` when the prompt_template row doesn't
/// exist (JS `null`).
pub fn getPromptName(db: &Connection, prompt_template_id: &Value) -> Option<Value> {
    let rows = crate::db_json::query_rows(db, "SELECT name, version FROM brain.prompt_templates WHERE id = ?", &[prompt_template_id.clone()]).ok()?;
    let r = rows.as_array().and_then(|a| a.first())?;
    Some(json!({ "name": r.get("name").cloned().unwrap_or(Value::Null), "version": r.get("version").cloned().unwrap_or(Value::Null) }))
}

/// `function diffOutputs(originalOutputs, replayOutputs)` — compares two `ai_call_outputs` row
/// arrays by `${field_name}:${order_index ?? 0}:${parent_output_id ?? 'top'}` key, value =
/// `value_text ?? value_number ?? value_boolean ?? null`. `allKeys` iterates in JS `Set`
/// insertion order: every key from `originalOutputs` (first-seen), then any NEW key first seen in
/// `replayOutputs`. Pure.
pub fn diffOutputs(original_outputs: &Value, replay_outputs: &Value) -> Value {
    fn idx(rows: &Value) -> (Vec<String>, std::collections::HashMap<String, Value>) {
        let mut order: Vec<String> = Vec::new();
        let mut map: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
        if let Some(arr) = rows.as_array() {
            for r in arr {
                let field_part = tmpl(r.get("field_name"));
                let order_part = tmpl(nz(r.get("order_index")).or(Some(&json!(0))));
                let parent_part = tmpl(nz(r.get("parent_output_id")).or(Some(&json!("top"))));
                let key = format!("{field_part}:{order_part}:{parent_part}");
                let value = nz(r.get("value_text"))
                    .or_else(|| nz(r.get("value_number")))
                    .or_else(|| nz(r.get("value_boolean")))
                    .cloned()
                    .unwrap_or(Value::Null);
                if !map.contains_key(&key) { order.push(key.clone()); }
                map.insert(key, value);
            }
        }
        (order, map)
    }
    let (a_order, a) = idx(original_outputs);
    let (b_order, b) = idx(replay_outputs);
    let mut all_keys = a_order.clone();
    let mut seen: std::collections::HashSet<String> = a_order.iter().cloned().collect();
    for k in &b_order {
        if seen.insert(k.clone()) { all_keys.push(k.clone()); }
    }
    let mut differences: Vec<Value> = Vec::new();
    for k in &all_keys {
        let av = a.get(k).cloned().unwrap_or(Value::Null);
        let bv = b.get(k).cloned().unwrap_or(Value::Null);
        if av != bv {
            differences.push(json!({ "key": k, "was": av, "now": bv }));
        }
    }
    json!({ "identical": differences.is_empty(), "differences": differences })
}

/// `export default async function aiCallReplay(input, context)` — guards + loads the original
/// call/inputs/prompt name, reconstructs `input_kv`, and dispatches the replay through
/// `crate::ai_ai_call::aiCall` (PARITY-DEFERRED — see module doc; always returns `Err` here, so
/// the JS tail past that call — `replay_of_id` tagging, output diffing, markdown rendering — is
/// unreachable in this twin. `diffOutputs` above ports that tail's pure comparison logic as its
/// own tested unit).
pub fn aiCallReplay(input: &Value, db: &Connection) -> Result<Value, String> {
    let ai_call_id = nz(input.get("ai_call_id")).cloned();
    if !truthy(ai_call_id.as_ref()) {
        return Err("ai-call-replay: ai_call_id is required".into());
    }
    let ai_call_id = ai_call_id.unwrap();
    let tier_override = nz(input.get("tier_override")).cloned();
    let temperature_override = nz(input.get("temperature_override")).cloned();

    let orig = match loadOriginalInputs(db, &ai_call_id) {
        Some(o) => o,
        None => return Err(format!("ai_call {} not found", tmpl(Some(&ai_call_id)))),
    };
    let inputs = orig.get("inputs").cloned().unwrap_or(json!([]));
    if inputs.as_array().map_or(true, |a| a.is_empty()) {
        let source_id = orig.get("source_id").cloned().unwrap_or(Value::Null);
        return Err(format!(
            "ai_call {} has no persisted inputs (pre-FEAT-AI-CALL-INPUTS-PERSISTED). Cannot replay deterministically. source_id={}",
            tmpl(Some(&ai_call_id)), tmpl(Some(&source_id))
        ));
    }
    let call = orig.get("call").cloned().unwrap_or(Value::Null);
    let prompt_template_id = call.get("prompt_template_id").cloned().unwrap_or(Value::Null);
    let prompt_info = match getPromptName(db, &prompt_template_id) {
        Some(p) => p,
        None => return Err(format!("prompt_template {} not found", tmpl(Some(&prompt_template_id)))),
    };

    // Reconstruct input_kv from the persisted rows — value stays the raw string (aiCall
    // canonicalizes/hashes the same way, so hashes match if values are unchanged).
    let input_kv: Vec<Value> = inputs.as_array().unwrap().iter().map(|r| {
        json!({ "key": r.get("key").cloned().unwrap_or(Value::Null), "value": r.get("value_text").cloned().unwrap_or(Value::Null) })
    }).collect();

    let tier = nz(tier_override.as_ref()).cloned().unwrap_or_else(|| call.get("tier").cloned().unwrap_or(Value::Null));
    let temperature = nz(temperature_override.as_ref()).cloned().unwrap_or_else(|| call.get("temperature").cloned().unwrap_or(Value::Null));

    let ai_input = json!({
        "prompt_name": prompt_info.get("name").cloned().unwrap_or(Value::Null),
        "prompt_version": prompt_info.get("version").cloned().unwrap_or(Value::Null),
        "input_kv": input_kv,
        "tier": tier,
        "temperature": temperature,
        "bypass_cache": true,
        "skip_feed_check": true,
        "provider_fn": Value::Null,
    });

    // PARITY-DEFERRED boundary: crate::ai_ai_call::aiCall is a network stub that always returns
    // Err — see module doc. Everything past this point in the real JS (replay_of_id UPDATE,
    // ai_call_outputs re-read + diffOutputs, output_diff_md rendering, final shaping) is
    // unreachable here.
    let replay = crate::ai_ai_call::aiCall(&ai_input, db)?;
    Ok(replay)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "
        CREATE TABLE brain.ai_calls ( id INTEGER PRIMARY KEY AUTOINCREMENT, cached INTEGER, cache_source_call_id INTEGER, prompt_template_id INTEGER, tier TEXT, temperature TEXT, replay_of_id INTEGER );
        CREATE TABLE brain.ai_call_inputs ( id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, key TEXT, value_text TEXT, order_index INTEGER );
        CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, version INTEGER );
        CREATE TABLE brain.ai_call_outputs ( id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, field_name TEXT, value_text TEXT, value_number INTEGER, value_boolean INTEGER, order_index INTEGER, parent_output_id INTEGER );
        -- promptFind (reached once aiCall stopped being a stub) reads both field tables; without
        -- them the replay fails with a bare 'no such table' instead of at the aiCall boundary.
        CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT );
        CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT );
    ";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    // ── loadOriginalInputs ───────────────────────────────────────────────
    #[test]
    fn load_original_inputs_missing_call_is_none() {
        let c = db();
        assert!(loadOriginalInputs(&c, &json!(999)).is_none());
    }

    #[test]
    fn load_original_inputs_non_cached_uses_own_id() {
        let c = db();
        c.execute_batch("INSERT INTO brain.ai_calls (id, cached, prompt_template_id, tier, temperature) VALUES (1, 0, 5, 'low', 'cold');
            INSERT INTO brain.ai_call_inputs (ai_call_id, key, value_text, order_index) VALUES (1, 'goal', 'do thing', 0);").unwrap();
        let r = loadOriginalInputs(&c, &json!(1)).unwrap();
        assert_eq!(r["source_id"], json!(1));
        assert_eq!(r["inputs"].as_array().unwrap().len(), 1);
        assert_eq!(r["inputs"][0]["key"], json!("goal"));
    }

    #[test]
    fn load_original_inputs_cached_follows_cache_source_call_id() {
        let c = db();
        c.execute_batch("INSERT INTO brain.ai_calls (id, cached, prompt_template_id) VALUES (7, 0, 5);
            INSERT INTO brain.ai_calls (id, cached, cache_source_call_id, prompt_template_id) VALUES (8, 1, 7, 5);
            INSERT INTO brain.ai_call_inputs (ai_call_id, key, value_text, order_index) VALUES (7, 'goal', 'original text', 0);").unwrap();
        let r = loadOriginalInputs(&c, &json!(8)).unwrap();
        assert_eq!(r["source_id"], json!(7));
        assert_eq!(r["inputs"][0]["value_text"], json!("original text"));
    }

    #[test]
    fn load_original_inputs_no_persisted_inputs_returns_empty_array() {
        let c = db();
        c.execute_batch("INSERT INTO brain.ai_calls (id, cached, prompt_template_id) VALUES (2, 0, 5);").unwrap();
        let r = loadOriginalInputs(&c, &json!(2)).unwrap();
        assert_eq!(r["inputs"], json!([]));
    }

    // ── getPromptName ────────────────────────────────────────────────────
    #[test]
    fn get_prompt_name_found_and_missing() {
        let c = db();
        c.execute_batch("INSERT INTO brain.prompt_templates (id, name, version) VALUES (5, 'blueprint.v1', 3);").unwrap();
        assert_eq!(getPromptName(&c, &json!(5)).unwrap(), json!({ "name": "blueprint.v1", "version": 3 }));
        assert!(getPromptName(&c, &json!(999)).is_none());
    }

    // ── diffOutputs ──────────────────────────────────────────────────────
    #[test]
    fn diff_outputs_identical_when_same_values() {
        let a = json!([{ "field_name": "x", "value_text": "hi", "order_index": 0 }]);
        let b = json!([{ "field_name": "x", "value_text": "hi", "order_index": 0 }]);
        let r = diffOutputs(&a, &b);
        assert_eq!(r["identical"], json!(true));
        assert_eq!(r["differences"], json!([]));
    }

    #[test]
    fn diff_outputs_flags_changed_value() {
        let a = json!([{ "field_name": "x", "value_text": "was", "order_index": 0 }]);
        let b = json!([{ "field_name": "x", "value_text": "now", "order_index": 0 }]);
        let r = diffOutputs(&a, &b);
        assert_eq!(r["identical"], json!(false));
        assert_eq!(r["differences"].as_array().unwrap().len(), 1);
        assert_eq!(r["differences"][0], json!({ "key": "x:0:top", "was": "was", "now": "now" }));
    }

    #[test]
    fn diff_outputs_key_defaults_and_missing_on_one_side() {
        // no order_index/parent_output_id → defaults 0/'top'; a key present only in replay.
        let a = json!([{ "field_name": "field_name", "value_text": "a" }]);
        let b = json!([
            { "field_name": "field_name", "value_text": "a" },
            { "field_name": "extra", "value_number": 5, "order_index": 1, "parent_output_id": 3 }
        ]);
        let r = diffOutputs(&a, &b);
        assert_eq!(r["identical"], json!(false));
        assert_eq!(r["differences"][0]["key"], json!("extra:1:3"));
        assert_eq!(r["differences"][0]["was"], Value::Null);
        assert_eq!(r["differences"][0]["now"], json!(5));
    }

    #[test]
    fn diff_outputs_value_precedence_text_then_number_then_boolean() {
        let a = json!([{ "field_name": "f", "value_boolean": 1, "order_index": 0 }]);
        let b = json!([{ "field_name": "f", "value_number": 7, "order_index": 0 }]);
        let r = diffOutputs(&a, &b);
        assert_eq!(r["differences"][0]["was"], json!(1));
        assert_eq!(r["differences"][0]["now"], json!(7));
    }

    // ── aiCallReplay guards + deferred boundary ─────────────────────────
    #[test]
    fn ai_call_replay_requires_ai_call_id() {
        let c = db();
        let e = aiCallReplay(&json!({}), &c).unwrap_err();
        assert!(e.contains("ai_call_id is required"));
    }

    #[test]
    fn ai_call_replay_not_found() {
        let c = db();
        let e = aiCallReplay(&json!({ "ai_call_id": 42 }), &c).unwrap_err();
        assert!(e.contains("ai_call 42 not found"));
    }

    #[test]
    fn ai_call_replay_no_persisted_inputs_errors() {
        let c = db();
        c.execute_batch("INSERT INTO brain.ai_calls (id, cached, prompt_template_id) VALUES (3, 0, 5);").unwrap();
        let e = aiCallReplay(&json!({ "ai_call_id": 3 }), &c).unwrap_err();
        assert!(e.contains("no persisted inputs"));
    }

    #[test]
    fn ai_call_replay_prompt_template_missing_errors() {
        let c = db();
        c.execute_batch("INSERT INTO brain.ai_calls (id, cached, prompt_template_id) VALUES (4, 0, 999);
            INSERT INTO brain.ai_call_inputs (ai_call_id, key, value_text, order_index) VALUES (4, 'goal', 'x', 0);").unwrap();
        let e = aiCallReplay(&json!({ "ai_call_id": 4 }), &c).unwrap_err();
        assert!(e.contains("prompt_template 999 not found"));
    }

    #[test]
    fn ai_call_replay_reaches_the_deferred_ai_call_boundary_on_valid_input() {
        let c = db();
        c.execute_batch("INSERT INTO brain.prompt_templates (id, name, version) VALUES (5, 'blueprint.v1', 3);
            INSERT INTO brain.ai_calls (id, cached, prompt_template_id, tier, temperature) VALUES (10, 0, 5, 'low', 'cold');
            INSERT INTO brain.ai_call_inputs (ai_call_id, key, value_text, order_index) VALUES (10, 'goal', 'do thing', 0);").unwrap();
        let e = aiCallReplay(&json!({ "ai_call_id": 10 }), &c).unwrap_err();
        assert!(e.contains("ai-call"), "expected the deferred ai_ai_call::aiCall stub error, got: {e}");
    }
}
