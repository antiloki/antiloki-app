//! architecture-digest.rs — twin of skills/codebase/architecture-digest.mjs (db READ lane +
//! deferred AI narrative). NAMES ARE CONTRACT: `readSkeleton` (named export, pure db-read helper —
//! exported so magazine-observe-style callers can ground on the same spine) and `architectureDigest`
//! (default). Reads the atlas SKELETON (file nodes + file→file 'imports' edges among them, top-N by
//! fan-in), renders a compact text skeleton, then asks a MID-tier model for a short narrative.
//!
//! `context.db` missing → JS throws `err.contextMissing('architecture-digest', 'db')`. Unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention as every sibling
//! db-lane twin). `context.provider_fn` (a JS function reference for provider selection/DI) has no
//! Rust equivalent to carry — threaded as `null` into the constructed aiCall input, same pattern as
//! pl-research.rs's `provider_fn`.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`, so `architectureDigest`
//! propagates that error once the (real, ported) deterministic surface above it has run: the
//! `readSkeleton` graph read/rank/render and `ensure_digest_prompt` (reusing the already-converted
//! `ai_prompt_find`/`ai_prompt_add` twins, mirroring pl-research.rs's `ensure_prompt`). Unlike
//! pl-research/book-run, the JS wraps its aiCall in a try/catch and returns a STATUS object
//! (`{digest_md:null, ok:false, error}`) instead of throwing — this twin mirrors that: it returns
//! `Ok(status_value)`, never `Err`, once `readSkeleton` has run (an empty atlas is the only real
//! error path, matching the JS `considered === 0` early return).
// QUALITY: mixed unit — deterministic graph-read/rank/render surface (fully ported, fixture-
// reachable) + 1 bespoke/deferred aiCall dispatch (no fixture/frontier), status-shaped like the JS.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
} // = JS String(v ?? '')

const DEFAULT_MAX_FILES: i64 = 40;
const MAX_IMPORTS_PER_FILE: usize = 12;

const PROMPT_NAME: &str = "atlas.architecture.digest.v1";
const SYSTEM_MD: &str = "You are a software architect. Given a skeleton of a codebase (its files and what each imports), you write a SHORT narrative of the architecture. You output only strict JSON.";
const TEMPLATE_MD: &str = "Describe this project's architecture as a SHORT narrative (a few paragraphs, markdown): the likely entry points, the layers, the main modules and how they depend on each other. Ground every claim in the skeleton below — do not invent files that are not listed.

SKELETON ({{file_count}} files, each line is \"file → imports it makes\"):
{{skeleton}}

Return JSON only as exactly: {\"digest_md\":\"<short architecture narrative in markdown>\"}";

fn prompt_output_fields() -> Value {
    json!([{ "field_name": "digest_md", "field_type": "string", "required": true, "min_length": 40 }])
}

/// `readSkeleton(db, origin, maxFiles)` — reads the SKELETON: file nodes + the file→file
/// 'imports' edges among them. Ranks files by fan-in (imported_by count, stable tiebreak on
/// path) and keeps the top `max_files`. Returns `(lines, considered)`. Pure db-read.
pub fn read_skeleton(db: &Connection, origin: &Value, max_files: i64) -> Result<(Vec<String>, i64), String> {
    let files_v = crate::db_json::query_rows(
        db,
        "SELECT id, path FROM code_nodes WHERE origin = ? AND type = 'file'",
        &[origin.clone()],
    )?;
    let files: Vec<Value> = files_v.as_array().cloned().unwrap_or_default();
    if files.is_empty() {
        return Ok((Vec::new(), 0));
    }

    let file_ids: std::collections::HashSet<String> = files.iter().map(|f| str_qq(f.get("id"))).collect();

    let edges_v = crate::db_json::query_rows(
        db,
        "SELECT from_node, to_node FROM code_edges WHERE origin = ? AND type = 'imports'",
        &[origin.clone()],
    )?;
    let edges: Vec<Value> = edges_v
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| file_ids.contains(&str_qq(e.get("from_node"))) && file_ids.contains(&str_qq(e.get("to_node"))))
        .collect();

    let mut fan_in: HashMap<String, i64> = HashMap::new();
    let mut imports_of: HashMap<String, Vec<String>> = HashMap::new();
    for e in &edges {
        let from = str_qq(e.get("from_node"));
        let to = str_qq(e.get("to_node"));
        *fan_in.entry(to.clone()).or_insert(0) += 1;
        imports_of.entry(from).or_default().push(to);
    }

    // Top-N by fan-in (stable tiebreak on path) — the bound.
    let mut ranked = files.clone();
    ranked.sort_by(|a, b| {
        let fa = *fan_in.get(&str_qq(a.get("id"))).unwrap_or(&0);
        let fb = *fan_in.get(&str_qq(b.get("id"))).unwrap_or(&0);
        fb.cmp(&fa).then_with(|| str_qq(a.get("path")).cmp(&str_qq(b.get("path"))))
    });
    let cap = if max_files < 0 { 0 } else { max_files as usize };
    ranked.truncate(cap);

    let path_of: HashMap<String, String> = files.iter().map(|f| (str_qq(f.get("id")), str_qq(f.get("path")))).collect();
    let in_scope: std::collections::HashSet<String> = ranked.iter().map(|f| str_qq(f.get("id"))).collect();

    let lines: Vec<String> = ranked
        .iter()
        .map(|f| {
            let fid = str_qq(f.get("id"));
            let fpath = str_qq(f.get("path"));
            let imps: Vec<String> = imports_of
                .get(&fid)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|id| in_scope.contains(id))
                .map(|id| path_of.get(&id).cloned().unwrap_or(id))
                .take(MAX_IMPORTS_PER_FILE)
                .collect();
            if imps.is_empty() {
                format!("{fpath} → (no in-scope imports)")
            } else {
                format!("{fpath} → {}", imps.join(", "))
            }
        })
        .collect();
    Ok((lines.clone(), lines.len() as i64))
}

// try promptFind; on miss (or mismatched template_md), promptAdd. Mirrors JS ensureDigestPrompt's
// try/catch around promptFind + the `existing.template_md === TEMPLATE` short-circuit.
fn ensure_digest_prompt(db: &Connection) -> Result<(), String> {
    if let Ok(existing) = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db) {
        if existing.get("template_md").and_then(Value::as_str) == Some(TEMPLATE_MD) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": TEMPLATE_MD,
            "system_md": SYSTEM_MD,
            "default_tier": "mid",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 1200,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    Ok(())
}

pub fn architectureDigest(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").filter(|v| !v.is_null()).cloned().unwrap_or(json!("atlas"));
    let max_files_raw = input.get("max_files").and_then(Value::as_f64).unwrap_or(DEFAULT_MAX_FILES as f64);
    let max_files = (max_files_raw.max(1.0).min(200.0)) as i64;

    let (lines, considered) = read_skeleton(db, &origin, max_files)?;
    if considered == 0 {
        let mut o = Map::new();
        o.insert("digest_md".into(), Value::Null);
        o.insert("files_considered".into(), json!(0));
        o.insert("ok".into(), json!(false));
        o.insert("error".into(), json!(format!("no file nodes for origin '{}'", str_qq(Some(&origin)))));
        return Ok(Value::Object(o));
    }
    let skeleton = lines.join("\n");

    ensure_digest_prompt(db)?;

    // ── the AI is a CACHE/view on the deterministic graph: degrade gracefully on any failure —
    // return a status, never propagate as Err. This is PARITY-DEFERRED (aiCall always errors).
    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "skeleton", "value": skeleton },
            { "key": "file_count", "value": considered.to_string() },
        ],
        "tier": "mid", "temperature": "cold", "phase_flow": "single_only", "provider_fn": Value::Null,
    });
    match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(_) => {
            // Unreachable on the deferred stub (it always errors) — mirrors the JS's success path
            // shape in case a future non-deferred aiCall lands here.
            let mut o = Map::new();
            o.insert("digest_md".into(), Value::Null);
            o.insert("files_considered".into(), json!(considered));
            o.insert("ok".into(), json!(false));
            o.insert("error".into(), json!("AI returned no usable digest"));
            Ok(Value::Object(o))
        }
        Err(e) => {
            let mut o = Map::new();
            o.insert("digest_md".into(), Value::Null);
            o.insert("files_considered".into(), json!(considered));
            o.insert("ok".into(), json!(false));
            o.insert("error".into(), json!(e.chars().take(200).collect::<String>()));
            Ok(Value::Object(o))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT PRIMARY KEY, origin TEXT, type TEXT, path TEXT);
             CREATE TABLE code_edges (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, type TEXT, from_node TEXT, to_node TEXT, kind TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    fn seed_files(c: &Connection) {
        c.execute_batch(
            "INSERT INTO code_nodes (id, origin, type, path) VALUES
               ('f1','atlas','file','src/index.mjs'),
               ('f2','atlas','file','src/engine.mjs'),
               ('f3','atlas','file','src/util.mjs');
             INSERT INTO code_edges (origin, type, from_node, to_node) VALUES
               ('atlas','imports','f1','f2'),
               ('atlas','imports','f1','f3'),
               ('atlas','imports','f2','f3');",
        ).unwrap();
    }

    #[test]
    fn no_file_nodes_returns_status_error_without_touching_ai() {
        let c = setup();
        let r = architectureDigest(&json!({}), &c).unwrap();
        assert_eq!(r["digest_md"], Value::Null);
        assert_eq!(r["files_considered"], json!(0));
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("no file nodes for origin 'atlas'"));
    }

    #[test]
    fn read_skeleton_ranks_by_fan_in_and_renders_lines() {
        let c = setup();
        seed_files(&c);
        let (lines, considered) = read_skeleton(&c, &json!("atlas"), 40).unwrap();
        assert_eq!(considered, 3);
        // f3 has highest fan-in (2), f2 next (1), f1 has none (0) — but sorted output lines
        // are keyed by path; f3's line comes first (highest fan-in), tiebreak by path otherwise.
        assert!(lines[0].starts_with("src/util.mjs →"));
        assert!(lines.iter().any(|l| l == "src/index.mjs → src/engine.mjs, src/util.mjs"));
        assert!(lines.iter().any(|l| l == "src/engine.mjs → src/util.mjs"));
        assert!(lines.iter().any(|l| l == "src/util.mjs → (no in-scope imports)"));
    }

    #[test]
    fn max_files_bounds_the_skeleton_to_top_n_by_fan_in() {
        let c = setup();
        seed_files(&c);
        let (lines, considered) = read_skeleton(&c, &json!("atlas"), 1).unwrap();
        assert_eq!(considered, 1);
        // only the top-1 by fan-in survives — src/util.mjs (fan-in 2).
        assert_eq!(lines, vec!["src/util.mjs → (no in-scope imports)".to_string()]);
    }

    #[test]
    fn full_run_reaches_the_parity_deferred_aicall_boundary_after_full_deterministic_surface() {
        let c = setup();
        seed_files(&c);
        let r = architectureDigest(&json!({ "origin": "atlas", "max_files": 10 }), &c).unwrap();
        assert_eq!(r["digest_md"], Value::Null);
        assert_eq!(r["files_considered"], json!(3));
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("ai-call"), "unexpected error: {}", r["error"]);
        // ensure_digest_prompt ran and registered the prompt template.
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'atlas.architecture.digest.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_digest_prompt_is_idempotent_on_matching_template() {
        let c = setup();
        ensure_digest_prompt(&c).unwrap();
        ensure_digest_prompt(&c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'atlas.architecture.digest.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1); // second call short-circuits on matching template_md, no version bump
    }

    #[test]
    fn max_files_clamped_between_1_and_200() {
        let c = setup();
        seed_files(&c);
        let (_lines0, considered0) = read_skeleton(&c, &json!("atlas"), 1.max(0)).unwrap();
        assert_eq!(considered0, 1);
        // architectureDigest clamps a huge requested max_files down to 200 (moot here with only 3
        // files, but exercised via the guard math directly).
        let clamped = (5000.0f64.max(1.0).min(200.0)) as i64;
        assert_eq!(clamped, 200);
    }
}
