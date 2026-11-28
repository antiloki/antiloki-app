//! ultra-concepts.rs — twin of skills/ultra/ultra-concepts.mjs (standard lane, aiCall/embed
//! deferred). NAMES ARE CONTRACT: `ultraConcepts` (default). THE CONCEPT ATLAS: per file, one
//! low-tier AI call names the 2-5 ideas the file embodies; everything else (content_hash caching
//! per law 4, near-duplicate label clustering, persistence) is deterministic and ported for real.
//!
//! PARITY-DEFERRED (two boundaries, both real dispatch points):
//!   · `crate::ai_ai_call::aiCall` — the per-file concept-extraction judge call. Reached for every
//!     `usable` file whose `content_hash` has no prior row in `judgments.ultra_concepts` (the cache
//!     miss path). Always `Err`s (see ai-call.rs's own doc) — ported as a `match` exactly mirroring
//!     the JS `try { res = await aiCall(...) } catch (e) { errors.push(...); concepts: [] }`: one
//!     bad file must not abort the batch.
//!   · `crate::external_embed::embed` — the local bge-base clustering pass, reached only when
//!     `names.len() > 1 && embedModelCached(DEFAULT_MODEL)`. `embed` always `Err`s once past its
//!     own `text` guard (real ONNX inference, no deterministic twin) — ported as a JS
//!     try/catch-the-whole-loop: the FIRST embed failure aborts clustering entirely (JS `for`
//!     loop inside one `try`, no per-iteration catch), pushed as `{stage:'cluster', error}`,
//!     `clustered` stays `false`. `embedModelCached` itself IS real (a filesystem probe) and is
//!     exercised with a real temp `HF_HOME` in the tests below — this is the ONE branch of this
//!     module where the deferred boundary's gate condition is independently, deterministically
//!     testable without needing the boundary itself to succeed.
//!
//! Everything AROUND both boundaries is real: run_id/files validation, `ensureUltraSchema`,
//! `ensureConceptPrompt` (reuses `ai_prompt_find`/`ai_prompt_add`, matching template_md AND
//! system_md per the JS `existing.template_md === TEMPLATE && existing.system_md === SYSTEM`
//! double-check — file-summarize.rs's sibling helper only checks template_md, not ported here),
//! the `usable` filter, per-file `content_hash` cache lookup (`LIMIT 8`, `run_id <> ?` so a file
//! never replays against its own in-flight run), `parseConcepts`/`normalizeName` (dead in practice
//! since `aiCall` never returns `Ok`, but ported faithfully — a future un-deferred `aiCall` makes
//! them live with zero further porting), the greedy single-pass cluster-head merge (`dot` = cosine
//! since bge vectors are L2-normalized), and the final canonical-name persistence loop.
//!
//! `context.db` missing → JS throws via `err.required`. Not modeled: `db: &Connection` is a
//! required, non-nullable reference here, same "guard-throws are not exercised" convention every
//! sibling twin uses. `run_id`/`files` ARE reachable with a valid db and ARE modeled as `Err`.
// QUALITY: real db read/write + caching/clustering/persistence logic; both AI dispatch edges
// PARITY-DEFERRED (aiCall unconditionally, embed unconditionally past its guard) — proven via
// #[cfg(test)] native tests, no conform-db fixture/frontier entry.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn str_qq(v: Option<&Value>) -> String {
    // = JS String(v ?? '')
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
} // JS `??` right operand
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse().unwrap_or(f64::NAN)
            }
        }
        Some(_) => f64::NAN,
    }
}

fn err_required(field: &str) -> String {
    format!("ultra-concepts: {field} is required")
}

const PROMPT: &str = "ultra-concepts";
const SYSTEM: &str = "You name the IDEAS a source file embodies — the concepts a senior engineer would use to explain what this file is FOR, not what its syntax is. Rules: (1) 2-5 concepts, fewer is better; a file with one idea gets one. (2) A concept is a short noun phrase in plain lowercase english (\"audit chain\", \"replay cache\", \"retry with backoff\") — NEVER a symbol name, a file name, or a generic word like \"utils\"/\"helpers\"/\"code\". (3) kind: \"domain\" (a business/product idea), \"mechanism\" (a technical device), or \"pattern\" (a recognised structure). (4) evidence MUST be a symbol name or short phrase copied from the file. (5) If the file embodies nothing nameable, return {\"concepts\":[]} — that is a complete answer. Output ONLY valid JSON: {\"concepts\":[{\"name\":\"...\",\"kind\":\"domain|mechanism|pattern\",\"evidence\":\"...\"}]}. No prose outside the JSON.";
const TEMPLATE: &str = "FILE: {{path}}\n\nSOURCE (may be truncated):\n{{source}}\n\nOutput the concepts JSON.";

const MAX_SOURCE: usize = 5000;
const SIM_THRESHOLD: f64 = 0.86; // cosine over bge-base: merges 'audit chain'/'audit trail'

fn ensure_concept_prompt(db: &Connection) -> Result<(), String> {
    if let Ok(existing) = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db) {
        let matches = existing.get("template_md").and_then(Value::as_str) == Some(TEMPLATE)
            && existing.get("system_md").and_then(Value::as_str) == Some(SYSTEM);
        if matches {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "low",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 500,
            "output_fields": [{ "field_name": "concepts_json", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

/// `JSON.parse(s.slice(s.indexOf('{'), s.lastIndexOf('}') + 1))?.concepts` — any parse failure
/// or missing brace pair degrades to an empty list, never throws.
fn parse_concepts(text: &str) -> Vec<Value> {
    let start = match text.find('{') {
        Some(i) => i,
        None => return Vec::new(),
    };
    let end = match text.rfind('}') {
        Some(i) if i > start => i,
        _ => return Vec::new(),
    };
    match serde_json::from_str::<Value>(&text[start..=end]) {
        Ok(v) => v.get("concepts").and_then(Value::as_array).cloned().unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

// a concept label is a plain lowercase noun phrase — never a symbol or a path
fn generic() -> &'static HashSet<&'static str> {
    use std::sync::OnceLock;
    static G: OnceLock<HashSet<&'static str>> = OnceLock::new();
    G.get_or_init(|| {
        ["utils", "helpers", "code", "logic", "functions", "module", "file", "stuff", "misc"]
            .into_iter()
            .collect()
    })
}

fn normalize_name(raw: Option<&Value>) -> Option<String> {
    let lowered = str_qq(raw).to_lowercase();
    // .replace(/[_/]+/g, ' ') — a run of _ or / collapses to ONE space
    let mut step1 = String::new();
    let mut in_sep_run = false;
    for ch in lowered.chars() {
        if ch == '_' || ch == '/' {
            if !in_sep_run {
                step1.push(' ');
                in_sep_run = true;
            }
        } else {
            step1.push(ch);
            in_sep_run = false;
        }
    }
    // .replace(/[^a-z0-9 +-]/g, '') — disallowed chars are DELETED, not replaced by space
    let step2: String = step1
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == ' ' || *c == '+' || *c == '-')
        .collect();
    // .replace(/\s+/g, ' ').trim()
    let n = step2.split_whitespace().collect::<Vec<_>>().join(" ");
    let len = n.chars().count();
    if n.is_empty() || len < 3 || len > 48 || generic().contains(n.as_str()) {
        None
    } else {
        Some(n)
    }
}

// bge vectors are L2-normalized → dot = cosine
fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

struct FileConcept {
    name: String,
    kind: String,
    evidence: Value,
    model: Value,
}
struct FileResult {
    path: Value,
    content_hash: Value,
    concepts: Vec<FileConcept>,
}

/// `export default async function ultraConcepts(input, context)`
pub fn ultraConcepts(input: &Value, db: &Connection) -> Result<Value, String> {
    let raw_run_id = js_number(input.get("run_id"));
    if raw_run_id.is_nan() || raw_run_id == 0.0 {
        return Err(err_required("run_id"));
    }
    let run_id = raw_run_id as i64;

    let files = match input.get("files").and_then(Value::as_array) {
        Some(a) => a.clone(),
        None => return Err(err_required("files")),
    };

    crate::ultra__helpers_schema::ensureUltraSchema(db);

    let usable: Vec<&Value> = files
        .iter()
        .filter(|f| {
            f.get("source").and_then(Value::as_str).map_or(false, |s| !s.trim().is_empty())
                && truthy(f.get("content_hash"))
        })
        .collect();

    if usable.is_empty() {
        return Ok(json!({ "calls": 0, "cost_usd": 0, "cached": 0, "concepts": 0, "clustered": false, "errors": [] }));
    }

    ensure_concept_prompt(db)?;

    let mut errors: Vec<Value> = Vec::new();
    let mut calls: i64 = 0;
    let mut cost: f64 = 0.0;
    let mut cached_files: i64 = 0;

    // ── extract (cached by content_hash — law 4) ───────────────────────────
    let mut results: Vec<FileResult> = Vec::new();
    for f in &usable {
        let path = f.get("path").cloned().unwrap_or(Value::Null);
        let content_hash = f.get("content_hash").cloned().unwrap_or(Value::Null);

        let hit = crate::db_json::query_rows(
            db,
            "SELECT name, kind, evidence, model FROM judgments.ultra_concepts WHERE content_hash = ? AND run_id <> ? LIMIT 8",
            &[content_hash.clone(), json!(run_id)],
        )?;
        let hit_rows = hit.as_array().cloned().unwrap_or_default();
        if !hit_rows.is_empty() {
            cached_files += 1;
            let concepts = hit_rows
                .iter()
                .map(|r| FileConcept {
                    name: str_qq(r.get("name")),
                    kind: str_qq(r.get("kind")),
                    evidence: r.get("evidence").cloned().unwrap_or(Value::Null),
                    model: r.get("model").cloned().unwrap_or(Value::Null),
                })
                .collect();
            results.push(FileResult { path, content_hash, concepts });
            continue;
        }

        let source = str_qq(f.get("source"));
        let truncated: String = source.chars().take(MAX_SOURCE).collect();
        let ai_input = json!({
            "prompt_name": PROMPT,
            "input_kv": [
                { "key": "path", "value": path.clone() },
                { "key": "source", "value": truncated },
            ],
            "provider_fn": Value::Null,
        });
        match crate::ai_ai_call::aiCall(&ai_input, db) {
            Ok(res) => {
                calls += 1;
                cost += js_number(nz(res.get("cost_usd")).or(Some(&json!(0))));
                let text = res
                    .get("output_kv")
                    .and_then(Value::as_array)
                    .and_then(|arr| {
                        arr.iter().find(|o| {
                            o.get("field_name").and_then(Value::as_str) == Some("concepts_json")
                                && matches!(o.get("parent_output_id"), None | Some(Value::Null))
                        })
                    })
                    .and_then(|o| o.get("value_text"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let model_id = res.get("model_id").cloned().unwrap_or(Value::Null);
                let concepts: Vec<FileConcept> = parse_concepts(&text)
                    .into_iter()
                    .filter_map(|c| {
                        let name = normalize_name(c.get("name"))?;
                        let kind = match c.get("kind").and_then(Value::as_str) {
                            Some(k) if ["domain", "mechanism", "pattern"].contains(&k) => k.to_string(),
                            _ => "mechanism".to_string(),
                        };
                        let evidence: String = str_qq(nz(c.get("evidence"))).chars().take(200).collect();
                        Some(FileConcept { name, kind, evidence: json!(evidence), model: model_id.clone() })
                    })
                    .take(5)
                    .collect();
                results.push(FileResult { path, content_hash, concepts });
            }
            Err(e) => {
                let msg: String = e.chars().take(200).collect();
                errors.push(json!({ "path": path, "error": msg }));
                results.push(FileResult { path, content_hash, concepts: Vec::new() });
            }
        }
    }

    // ── cluster near-duplicate labels — LOCAL embeddings, $0 API ──────────
    // Fail closed: if the model is not already on disk, exact labels only (a run must never
    // stall on a 140MB download).
    let mut names: Vec<String> = Vec::new();
    {
        let mut seen: HashSet<String> = HashSet::new();
        for r in &results {
            for c in &r.concepts {
                if seen.insert(c.name.clone()) {
                    names.push(c.name.clone());
                }
            }
        }
    }
    let mut canonical: HashMap<String, String> = names.iter().map(|n| (n.clone(), n.clone())).collect();
    let mut clustered = false;

    if names.len() > 1 && crate::external_embed::embedModelCached(crate::external_embed::DEFAULT_MODEL) {
        let mut vecs: HashMap<String, Vec<f64>> = HashMap::new();
        let cluster_result: Result<(), String> = (|| {
            for n in &names {
                let v = crate::external_embed::embed(&json!({ "text": n }), &Value::Null)?;
                if let Some(arr) = v.get("vector").and_then(Value::as_array) {
                    let vec_f64: Vec<f64> = arr.iter().filter_map(Value::as_f64).collect();
                    vecs.insert(n.clone(), vec_f64);
                }
            }
            Ok(())
        })();
        match cluster_result {
            Ok(()) => {
                // greedy single-pass merge: longest labels first act as cluster heads
                let mut sorted = names.clone();
                sorted.sort_by(|a, b| b.chars().count().cmp(&a.chars().count()).then_with(|| a.cmp(b)));
                let mut heads: Vec<String> = Vec::new();
                for n in &sorted {
                    let v = match vecs.get(n) {
                        Some(v) => v,
                        None => continue,
                    };
                    let near = heads.iter().find(|h| {
                        vecs.get(*h).map_or(false, |hv| dot(hv, v) >= SIM_THRESHOLD)
                    });
                    match near {
                        Some(h) => {
                            canonical.insert(n.clone(), h.clone());
                        }
                        None => heads.push(n.clone()),
                    }
                }
                clustered = true;
            }
            Err(e) => {
                let msg: String = e.chars().take(200).collect();
                errors.push(json!({ "stage": "cluster", "error": msg }));
            }
        }
    }

    // ── persist ──────────────────────────────────────────────────────────
    let mut persisted: i64 = 0;
    for r in &results {
        for c in &r.concepts {
            let canon_name = canonical.get(&c.name).cloned().unwrap_or_else(|| c.name.clone());
            crate::db_json::run_write(
                db,
                "INSERT INTO judgments.ultra_concepts (run_id, path, content_hash, name, kind, evidence, model) VALUES (?, ?, ?, ?, ?, ?, ?)",
                &[
                    json!(run_id),
                    r.path.clone(),
                    r.content_hash.clone(),
                    json!(canon_name),
                    json!(c.kind.clone()),
                    c.evidence.clone(),
                    c.model.clone(),
                ],
            )?;
            persisted += 1;
        }
    }

    let cluster_note = if clustered {
        "near-duplicate labels merged via local embeddings ($0)"
    } else {
        "exact-label matching only — the local embedding model is not cached on this machine (no mid-run download)"
    };

    let mut out = Map::new();
    out.insert("calls".into(), json!(calls));
    out.insert("cost_usd".into(), json!(cost));
    out.insert("cached".into(), json!(cached_files));
    out.insert("concepts".into(), json!(persisted));
    out.insert("clustered".into(), json!(clustered));
    out.insert("cluster_note".into(), json!(cluster_note));
    out.insert("errors".into(), json!(errors));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn with_hf_home<T>(hf_home: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("HF_HOME", hf_home);
        }
        let result = f();
        unsafe {
            std::env::remove_var("HF_HOME");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-ultra-concepts-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE judgments.ultra_concepts (id INTEGER PRIMARY KEY AUTOINCREMENT, run_id INTEGER NOT NULL, \
             path TEXT NOT NULL, content_hash TEXT NOT NULL, name TEXT NOT NULL, kind TEXT NOT NULL DEFAULT 'mechanism', \
             evidence TEXT, model TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00'))); \
             CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, \
             template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, \
             max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, \
             created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, \
             judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version)); \
             CREATE TABLE brain.prompt_output_fields (id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, \
             field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, \
             description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, \
             required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name)); \
             CREATE TABLE brain.prompt_input_fields (id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, \
             field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, \
             description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, \
             required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name));",
        )
        .unwrap();
        c
    }

    #[test]
    fn run_id_is_required() {
        let c = db();
        let e = ultraConcepts(&json!({ "files": [] }), &c).unwrap_err();
        assert_eq!(e, "ultra-concepts: run_id is required");
        let e0 = ultraConcepts(&json!({ "run_id": 0, "files": [] }), &c).unwrap_err();
        assert_eq!(e0, "ultra-concepts: run_id is required");
    }

    #[test]
    fn files_is_required() {
        let c = db();
        let e = ultraConcepts(&json!({ "run_id": 1 }), &c).unwrap_err();
        assert_eq!(e, "ultra-concepts: files is required");
    }

    #[test]
    fn no_usable_files_short_circuits() {
        let c = db();
        let r = ultraConcepts(
            &json!({ "run_id": 1, "files": [{ "path": "a.mjs", "source": "   " }, { "path": "b.mjs", "source": "x" }] }),
            &c,
        )
        .unwrap();
        assert_eq!(r["calls"], json!(0));
        assert_eq!(r["cached"], json!(0));
        assert_eq!(r["concepts"], json!(0));
        assert_eq!(r["clustered"], json!(false));
        assert_eq!(r["errors"], json!([]));
    }

    #[test]
    fn cache_hit_replays_free_and_persists_into_the_new_run() {
        let c = db();
        c.execute(
            "INSERT INTO judgments.ultra_concepts (run_id, path, content_hash, name, kind, evidence, model) \
             VALUES (7, 'old/path.mjs', 'hash-A', 'audit chain', 'mechanism', 'AuditLog', 'sonnet')",
            [],
        )
        .unwrap();

        let r = ultraConcepts(
            &json!({ "run_id": 9, "files": [{ "path": "new/path.mjs", "source": "// real code", "content_hash": "hash-A" }] }),
            &c,
        )
        .unwrap();

        assert_eq!(r["calls"], json!(0)); // pure cache hit, no aiCall dispatch
        assert_eq!(r["cached"], json!(1));
        assert_eq!(r["concepts"], json!(1));
        assert_eq!(r["errors"], json!([]));

        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM judgments.ultra_concepts WHERE run_id = 9 AND path = 'new/path.mjs' AND name = 'audit chain'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn cache_lookup_excludes_rows_from_the_same_run_id() {
        let c = db();
        c.execute(
            "INSERT INTO judgments.ultra_concepts (run_id, path, content_hash, name, kind, evidence, model) \
             VALUES (5, 'x.mjs', 'hash-B', 'replay cache', 'mechanism', 'cache', NULL)",
            [],
        )
        .unwrap();

        // same run_id as the pre-existing row -> `run_id <> ?` excludes it -> falls through to
        // the deferred aiCall dispatch, which always Errs.
        let r = ultraConcepts(
            &json!({ "run_id": 5, "files": [{ "path": "y.mjs", "source": "// code", "content_hash": "hash-B" }] }),
            &c,
        )
        .unwrap();
        assert_eq!(r["cached"], json!(0));
        assert_eq!(r["errors"].as_array().unwrap().len(), 1);
        assert_eq!(r["errors"][0]["path"], json!("y.mjs"));
    }

    #[test]
    fn cache_miss_dispatches_deferred_ai_call_and_swallows_its_error_per_file() {
        let c = db();
        let r = ultraConcepts(
            &json!({
                "run_id": 1,
                "files": [
                    { "path": "a.mjs", "source": "// a", "content_hash": "ha" },
                    { "path": "b.mjs", "source": "// b", "content_hash": "hb" },
                ]
            }),
            &c,
        )
        .unwrap();
        assert_eq!(r["calls"], json!(0)); // aiCall never reaches Ok
        assert_eq!(r["cached"], json!(0));
        assert_eq!(r["concepts"], json!(0));
        assert_eq!(r["clustered"], json!(false));
        let errs = r["errors"].as_array().unwrap();
        assert_eq!(errs.len(), 2); // one bad file per file, batch keeps going
        assert_eq!(errs[0]["path"], json!("a.mjs"));
        assert_eq!(errs[1]["path"], json!("b.mjs"));

        // the prompt row was still ensured before any dispatch attempt.
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'ultra-concepts'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_concept_prompt_is_idempotent() {
        let c = db();
        ensure_concept_prompt(&c).unwrap();
        ensure_concept_prompt(&c).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'ultra-concepts'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn normalize_name_rejects_generic_and_too_short_labels() {
        assert_eq!(normalize_name(Some(&json!("audit_chain"))), Some("audit chain".to_string()));
        assert_eq!(normalize_name(Some(&json!("utils"))), None); // generic
        assert_eq!(normalize_name(Some(&json!("ab"))), None); // too short
        assert_eq!(normalize_name(Some(&json!("Retry With Backoff!"))), Some("retry with backoff".to_string()));
        assert_eq!(normalize_name(None), None);
    }

    #[test]
    fn parse_concepts_extracts_the_json_object_between_first_and_last_brace() {
        let got = parse_concepts("noise{\"concepts\":[{\"name\":\"x\"}]}trailing");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["name"], json!("x"));
        assert_eq!(parse_concepts("not json at all"), Vec::<Value>::new());
        assert_eq!(parse_concepts("{ malformed"), Vec::<Value>::new());
    }

    #[test]
    fn dot_product_is_cosine_over_l2_normalized_vectors() {
        assert_eq!(dot(&[1.0, 0.0], &[1.0, 0.0]), 1.0);
        assert_eq!(dot(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
    }

    #[test]
    fn two_or_more_names_with_cached_model_reach_the_deferred_embed_and_report_a_cluster_error() {
        let dir = temp_dir("cluster-hit");
        let model = crate::external_embed::DEFAULT_MODEL;
        let hub_dir = dir.join("hub").join(format!("models--{}", model.replace('/', "--")));
        fs::create_dir_all(&hub_dir).unwrap();

        let r = with_hf_home(dir.to_str().unwrap(), || {
            let c = db();
            c.execute(
                "INSERT INTO judgments.ultra_concepts (run_id, path, content_hash, name, kind, evidence, model) \
                 VALUES (100, 'p1.mjs', 'h1', 'audit chain', 'mechanism', 'e1', NULL)",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO judgments.ultra_concepts (run_id, path, content_hash, name, kind, evidence, model) \
                 VALUES (100, 'p2.mjs', 'h2', 'replay cache', 'mechanism', 'e2', NULL)",
                [],
            )
            .unwrap();

            ultraConcepts(
                &json!({
                    "run_id": 1,
                    "files": [
                        { "path": "p1.mjs", "source": "// a", "content_hash": "h1" },
                        { "path": "p2.mjs", "source": "// b", "content_hash": "h2" },
                    ]
                }),
                &c,
            )
            .unwrap()
        });

        assert_eq!(r["clustered"], json!(false)); // embed always Errs -> cluster try aborts
        let errs = r["errors"].as_array().unwrap();
        assert!(errs.iter().any(|e| e.get("stage") == Some(&json!("cluster"))), "{errs:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_below_two_never_reach_the_cluster_branch_even_with_model_cached() {
        let dir = temp_dir("single-name-skip");
        let model = crate::external_embed::DEFAULT_MODEL;
        let hub_dir = dir.join("hub").join(format!("models--{}", model.replace('/', "--")));
        fs::create_dir_all(&hub_dir).unwrap();

        let r = with_hf_home(dir.to_str().unwrap(), || {
            let c = db();
            c.execute(
                "INSERT INTO judgments.ultra_concepts (run_id, path, content_hash, name, kind, evidence, model) \
                 VALUES (100, 'p1.mjs', 'h1', 'audit chain', 'mechanism', 'e1', NULL)",
                [],
            )
            .unwrap();

            ultraConcepts(
                &json!({ "run_id": 1, "files": [{ "path": "p1.mjs", "source": "// a", "content_hash": "h1" }] }),
                &c,
            )
            .unwrap()
        });

        assert_eq!(r["clustered"], json!(false));
        assert_eq!(r["errors"], json!([])); // no cluster attempt at all -> no cluster error either
        let _ = fs::remove_dir_all(&dir);
    }
}
