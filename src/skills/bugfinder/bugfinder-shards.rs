//! bugfinder-shards.rs — twin of skills/bugfinder/bugfinder-shards.mjs (db, cross-db brain.*).
//! NAMES ARE CONTRACT: default `bugfinderShards`. WAVE 2 of the bugfinder swarm: one LOW-tier AI
//! call per (file × applicable SHARD — an orthogonal bug species). Test files get only the
//! `test-oracle` shard; source files get everything else. Every lead a shard reports must be
//! deterministically grounded against the file's own text (`grounding.rs`'s `parseFindingsJson`/
//! `validateLead`, already-converted, reused verbatim below) — dropped leads are RETURNED, not
//! discarded, so the caller keeps the whole precision ledger.
//!
//! PARITY-DEFERRED: each task's AI call dispatches through `crate::ai_ai_call::aiCall`, the
//! deferred network-AI stub (see ai-call.rs's module doc) — it always returns `Err`. Unlike
//! book-run.rs/pl-research.rs, the JS here `try/catch`es EACH task's `aiCall` individually and
//! pushes the failure into the `errors` array rather than letting it propagate — so `bugfinderShards`
//! itself is FULLY testable end-to-end with this twin: `shards`/`calls`/`cost_usd`/`kept`/`dropped`
//! all resolve deterministically (`calls`/`cost_usd` stay 0 and `kept`/`dropped` stay empty because
//! the JS only increments/pushes AFTER a successful `aiCall`, which the deferred stub never
//! produces), and `errors` collects one entry per (file × shard) task naming the deferred-stub
//! message. `SHARDS`'s per-species `hunts` text, `shardsForFile`'s test/source routing, and
//! `ensureShardPrompt` (reusing the already-converted `ai_prompt_find`/`ai_prompt_add` twins,
//! mirroring book-run.rs's `ensure_faculty`) are ported byte-for-byte and are the reachable
//! deterministic surface. JS's `inChunks(tasks, 4, fn)` batches 4 tasks concurrently via
//! `Promise.all`, but since `Promise.all` preserves input order in its resolved array and each
//! task's outcome is independent (no shared mutable state raced — `kept`/`dropped`/`errors`/`cost`/
//! `calls` are appended synchronously per awaited task, not inside the parallel `map`), a plain
//! sequential `for` loop over `tasks` in the same order reproduces the identical `errors` order;
//! concurrency itself has no observable effect on the output.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
} // = JS String(v ?? '')

struct Shard {
    hunts: &'static str,
    applies_to_test: bool,
}

fn shards() -> &'static [(&'static str, Shard)] {
    // insertion order = Object.keys(SHARDS) order in the JS source — matters for the default
    // `enabled` list and its echo in the output's `shards` field.
    &[
        ("checking", Shard {
            hunts: "missing null/undefined/empty checks · unhandled error paths (rejected promises, throwing calls with no catch where one is needed) · missing boundary conditions (0, -1, max, empty array/string) · unvalidated external input · missing corner cases the surrounding code implies",
            applies_to_test: false,
        }),
        ("algorithm", Shard {
            hunts: "off-by-one errors · inverted or wrong conditions · wrong operator (&&/||, </<=, ==/===) · wrong variable used where a sibling was meant · incorrect loop bounds or accumulation · logic that contradicts the function's own name/comments",
            applies_to_test: false,
        }),
        ("timing", Shard {
            hunts: "unawaited promises whose result matters · await inside a loop that must be sequentialized or deliberately parallelized · shared-state mutation across async boundaries · missing cleanup/close/release on early return or error · event/callback ordering assumptions",
            applies_to_test: false,
        }),
        ("interface", Shard {
            hunts: "call-site vs signature mismatches visible in this file · wrong argument order or count · wrong shapes passed or returned vs how the result is used · optional fields accessed as required · contract drift between caller and callee in the same file",
            applies_to_test: false,
        }),
        ("security", Shard {
            hunts: "injection (SQL/command/path built by string concatenation from input) · missing authorization/permission checks before a privileged act · unbounded resource consumption (no cap, no limit, no timeout) · secrets or tokens embedded in code · unsafe deserialization of external data",
            applies_to_test: false,
        }),
        ("test-oracle", Shard {
            hunts: "assertions that encode what the implementation DOES rather than what the spec REQUIRES · tautological assertions (asserting a value against itself or a copy of the code under test) · magic expected-values pasted from observed output · missing negative-path/error-path assertions for behavior the test file clearly owns",
            applies_to_test: true,
        }),
    ]
}

fn shard_by_name(name: &str) -> Option<&'static Shard> {
    shards().iter().find(|(n, _)| *n == name).map(|(_, s)| s)
}

const MAX_SOURCE_CHARS: usize = 16_000;
const MAX_LEADS_PER_CALL: usize = 5;

const PROMPT: &str = "bugfinder-shard";
const SYSTEM: &str = "You are a NARROW bug finder hunting EXACTLY ONE defect category in one file. Rules: (1) Report ONLY defects in your category — anything else is out of scope even if real. (2) {\"findings\":[]} is a complete, correct answer — most files have NO defects in any single category; NEVER invent a finding to seem useful. (3) Every finding MUST quote the offending code VERBATIM in \"evidence\" — copied exactly from the file; evidence that does not appear in the file is discarded automatically. (4) Max 5 findings, highest confidence first. Output ONLY valid JSON: {\"findings\":[{\"line\":N,\"severity\":\"low|medium|high\",\"detail\":\"one sentence: what is wrong and when it breaks\",\"evidence\":\"VERBATIM code from the file\"}]}. No prose outside the JSON.";
const TEMPLATE: &str = "CATEGORY: {{shard}}\nHUNT ONLY: {{hunts}}\n\nFILE: {{file}}\n<file>\n{{source}}\n</file>\n\nOutput the findings JSON.";

// try promptFind; on hit with matching template_md/system_md, no-op; else promptAdd (mirrors JS
// ensureShardPrompt's try/catch — a mismatched existing template is intentionally left as-is,
// same as book-run.rs's ensure_faculty pattern except it also checks the fetched content).
fn ensure_shard_prompt(db: &Connection) -> Result<(), String> {
    if let Ok(existing) = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db) {
        let tmpl_ok = existing.get("template_md").and_then(|v| v.as_str()) == Some(TEMPLATE);
        let sys_ok = existing.get("system_md").and_then(|v| v.as_str()) == Some(SYSTEM);
        if tmpl_ok && sys_ok {
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
            "max_output_tokens": 1200,
            "output_fields": [{ "field_name": "findings_json", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

const TEST_FILE_RE: &str = r"(\.(test|spec)\.[a-z]+$)|(^|/)(__tests__|tests?)/";

fn is_test_path(path: &str) -> bool {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = R.get_or_init(|| regex::Regex::new(TEST_FILE_RE).unwrap());
    re.is_match(&path.replace('\\', "/"))
}

pub fn shardsForFile(path: &Value, enabled: &Value) -> Value {
    let is_test = is_test_path(&str_qq(Some(path)));
    let out: Vec<Value> = enabled
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| {
            let name = str_qq(Some(s));
            shard_by_name(&name).map_or(false, |sh| sh.applies_to_test == is_test)
        })
        .collect();
    json!(out)
}

struct Task {
    shard: String,
    file: String,
    source: String,
}

pub fn bugfinderShards(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `Array.isArray(input?.files) ? input.files : null` → throw err.required(...,'files').
    let files = match input.get("files") {
        Some(Value::Array(a)) => a.clone(),
        _ => return Err("bugfinder-shards: files is required".to_string()),
    };
    // JS: `const db = context?.db; if (!db) throw err.required(...,'context.db')` — unreachable
    // here (`db: &Connection` is never absent), same convention as every sibling db-lane twin.

    let shard_names: Vec<&'static str> = shards().iter().map(|(n, _)| *n).collect();
    let enabled: Vec<String> = match input.get("shards") {
        Some(Value::Array(a)) if !a.is_empty() => a
            .iter()
            .filter_map(|s| {
                let name = str_qq(Some(s));
                shard_by_name(&name).map(|_| name)
            })
            .collect(),
        _ => shard_names.iter().map(|s| s.to_string()).collect(),
    };
    ensure_shard_prompt(db)?;

    let mut tasks: Vec<Task> = Vec::new();
    for f in &files {
        let source = f.get("source");
        let is_str = matches!(source, Some(Value::String(_)));
        let src = if is_str { str_qq(source) } else { String::new() };
        if !is_str || src.trim().is_empty() {
            continue;
        }
        let path = str_qq(f.get("path"));
        let enabled_v = json!(enabled.clone());
        let applicable = shardsForFile(&json!(path), &enabled_v);
        for shard in applicable.as_array().cloned().unwrap_or_default() {
            tasks.push(Task { shard: str_qq(Some(&shard)), file: path.clone(), source: src.clone() });
        }
    }

    let mut kept: Vec<Value> = Vec::new();
    let mut dropped: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    let mut cost = 0.0f64;
    let mut calls: i64 = 0;

    for t in &tasks {
        let sh = shard_by_name(&t.shard).expect("task shard always comes from `enabled`, which was already filtered to known shard names");
        let source_trunc: String = t.source.chars().take(MAX_SOURCE_CHARS).collect();
        let ai_input = json!({
            "prompt_name": PROMPT,
            "input_kv": [
                { "key": "shard", "value": t.shard },
                { "key": "hunts", "value": sh.hunts },
                { "key": "file", "value": t.file },
                { "key": "source", "value": source_trunc },
            ],
            "provider_fn": Value::Null,
        });
        match crate::ai_ai_call::aiCall(&ai_input, db) {
            Ok(res) => {
                calls += 1;
                cost += res.get("cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let text = res
                    .get("output_kv")
                    .and_then(|v| v.as_array())
                    .and_then(|a| {
                        a.iter().find(|o| {
                            o.get("field_name").and_then(|v| v.as_str()) == Some("findings_json")
                                && o.get("parent_output_id").map_or(true, |v| v.is_null())
                        })
                    })
                    .and_then(|o| o.get("value_text"))
                    .cloned()
                    .unwrap_or(json!(""));
                let model_id = res.get("model_id").cloned().unwrap_or(Value::Null);
                let findings = crate::bugfinder__helpers_grounding::parseFindingsJson(&text);
                for raw in findings.as_array().cloned().unwrap_or_default().into_iter().take(MAX_LEADS_PER_CALL) {
                    let v = crate::bugfinder__helpers_grounding::validateLead(&raw, &json!(t.source));
                    let lead = v.get("lead").cloned().unwrap_or(Value::Null);
                    let ok = v.get("ok") == Some(&Value::Bool(true));
                    let mut base = Map::new();
                    base.insert("category".into(), json!(t.shard));
                    base.insert("severity".into(), lead.get("severity").cloned().unwrap_or(Value::Null));
                    base.insert("file".into(), json!(t.file));
                    base.insert("line".into(), lead.get("line").cloned().unwrap_or(Value::Null));
                    let detail = lead.get("detail").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
                    base.insert("detail".into(), json!(detail.unwrap_or("(no detail)")));
                    base.insert("evidence".into(), lead.get("evidence").cloned().unwrap_or(Value::Null));
                    base.insert("model".into(), model_id.clone());
                    if ok {
                        kept.push(Value::Object(base));
                    } else {
                        base.insert("drop_reason".into(), v.get("reason").cloned().unwrap_or(Value::Null));
                        dropped.push(Value::Object(base));
                    }
                }
            }
            Err(e) => {
                let msg: String = e.chars().take(300).collect();
                errors.push(json!({ "shard": t.shard, "file": t.file, "error": msg }));
            }
        }
    }

    Ok(json!({
        "shards": enabled,
        "calls": calls,
        "cost_usd": cost,
        "kept": kept,
        "dropped": dropped,
        "errors": errors,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn files_required() {
        let c = setup();
        let err = bugfinderShards(&json!({}), &c).unwrap_err();
        assert!(err.contains("files is required"), "{err}");
    }

    #[test]
    fn shards_for_file_routes_test_vs_source() {
        let enabled = json!(["checking", "algorithm", "test-oracle"]);
        let src = shardsForFile(&json!("skills/foo.mjs"), &enabled);
        assert_eq!(src, json!(["checking", "algorithm"]));
        let test = shardsForFile(&json!("skills/foo.test.mjs"), &enabled);
        assert_eq!(test, json!(["test-oracle"]));
        let nested = shardsForFile(&json!("skills/__tests__/bar.mjs"), &enabled);
        assert_eq!(nested, json!(["test-oracle"]));
    }

    #[test]
    fn defaults_to_all_shards_and_ensures_the_prompt() {
        let c = setup();
        let r = bugfinderShards(&json!({ "files": [] }), &c).unwrap();
        assert_eq!(
            r["shards"],
            json!(["checking", "algorithm", "timing", "interface", "security", "test-oracle"])
        );
        assert_eq!(r["calls"], json!(0));
        assert_eq!(r["cost_usd"], json!(0.0));
        assert_eq!(r["kept"], json!([]));
        assert_eq!(r["dropped"], json!([]));
        assert_eq!(r["errors"], json!([]));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'bugfinder-shard'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn skips_blank_source_files_and_filters_unknown_shard_names() {
        let c = setup();
        let r = bugfinderShards(
            &json!({
                "files": [{ "path": "a.mjs", "source": "  " }, { "path": "b.mjs" }],
                "shards": ["checking", "bogus"],
            }),
            &c,
        )
        .unwrap();
        assert_eq!(r["shards"], json!(["checking"]));
        assert_eq!(r["errors"], json!([]));
    }

    #[test]
    fn every_applicable_task_lands_a_deferred_aicall_error_and_prompt_is_ensured_once() {
        let c = setup();
        let r = bugfinderShards(
            &json!({
                "files": [
                    { "path": "src/a.mjs", "source": "const x = 1;" },
                    { "path": "src/a.test.mjs", "source": "test('x', () => {});" },
                ],
                "shards": ["checking", "algorithm", "test-oracle"],
            }),
            &c,
        )
        .unwrap();
        // a.mjs (source, 2 applicable shards) + a.test.mjs (test, 1 applicable shard) = 3 tasks
        let errors = r["errors"].as_array().unwrap();
        assert_eq!(errors.len(), 3);
        assert_eq!(errors[0]["shard"], json!("checking"));
        assert_eq!(errors[0]["file"], json!("src/a.mjs"));
        assert!(errors[0]["error"].as_str().unwrap().contains("ai-call") || errors[0]["error"].as_str().unwrap().contains("ai-call twin"));
        assert_eq!(errors[1]["shard"], json!("algorithm"));
        assert_eq!(errors[2]["shard"], json!("test-oracle"));
        assert_eq!(errors[2]["file"], json!("src/a.test.mjs"));
        assert_eq!(r["kept"], json!([]));
        assert_eq!(r["dropped"], json!([]));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'bugfinder-shard'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }
}
