//! atlas-file-snapshot.rs — twin of skills/codebase/atlas-file-snapshot.mjs (db, cross-db
//! code.*/brain.* attach lane). NAMES ARE CONTRACT: default `atlasFileSnapshot`. The per-file
//! "intelligence zoom": an AI analysis of ONE file against a FIXED JSON template, PERSISTED to
//! code.code_file_snapshots (latest wins; content_hash tracks staleness). Grounds the prompt in
//! the real source (via the already-converted `codebaseFileRead` twin) plus atlas signals
//! (defs, fan-in/out/complexity/risk from the already-converted `atlasMetrics`/`atlasScore`
//! twins, and smells from the already-converted `atlasSmells` twin) — each best-effort, matching
//! the JS's individual try/catch-per-signal (an off-atlas file still gets analyzed on source
//! alone).
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`, so `atlasFileSnapshot`
//! surfaces that as `{ ok: false, path, error: "analysis failed: ..." }` once the (real, ported)
//! deterministic surface above it has run: guard, source read, the three best-effort signal
//! gathers, `signals_md` assembly, and `ensure_prompt` (reusing the already-converted
//! `ai_prompt_find`/`ai_prompt_add` twins, mirroring pl-research.rs/book-run.rs's ensure-prompt
//! pattern). The response-parse (`pick`) and output-shaping (`normalize`) helpers are ported as
//! their own PURE functions and unit-tested directly with canned values — unreachable from
//! `atlasFileSnapshot` itself (the `?`-equivalent early-return on `aiCall`'s `Err` short-circuits
//! before either runs) but correct by construction, same pattern as pl-research.rs's
//! `build_research_report`/`build_sources`.
//!
//! `context.db` missing → JS throws `err.contextMissing('atlas-file-snapshot', 'db')`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (same convention as
//! every sibling db-lane twin). `context.provider_fn` (a JS function reference for provider
//! selection/DI) has no Rust equivalent to carry — threaded as `null` into the constructed
//! aiCall input, same as pl-research.rs's/book-run.rs's `provider_fn`.
// QUALITY: mixed unit — deterministic assembly/db-read/persist surface (fully ported,
// fixture-reachable up to the deferred aiCall boundary) + 2 pure response-shaping helpers
// (unreachable from the default export, unit-tested directly) + 1 bespoke/deferred aiCall
// dispatch (no fixture/frontier).

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
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
} // = JS String(v ?? '')
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

const PROMPT_NAME: &str = "atlas.file.snapshot.v1";
const SYSTEM_MD: &str = "You are a senior engineer producing a STRUCTURED per-file code analysis. You output ONLY one strict JSON object matching the requested template EXACTLY (same keys every time). Ground every statement in the source + signals given; invent nothing. If a section has nothing to report, return an empty array — never omit a key.";
const TEMPLATE_MD: &str = "Analyze the single source file below and return ONE strict JSON object, EXACTLY this template (same keys, same types, every time):

{
  \"description\": \"<1-2 sentences: what this file does and why it exists>\",
  \"health\": <integer 0-100, higher = healthier>,
  \"complexity\": \"<low|medium|high>\",
  \"responsibilities\": [\"<one responsibility>\", \"...\"],
  \"insights\": [\"<a non-obvious observation about design, coupling, or intent>\", \"...\"],
  \"vulnerabilities\": [{\"severity\": \"<critical|high|medium|low>\", \"issue\": \"<what + why it's risky>\", \"where\": \"<function name or line hint>\"}],
  \"smells\": [\"<a concrete code smell>\", \"...\"],
  \"suggestions\": [\"<a concrete, actionable improvement>\", \"...\"]
}

Rules: output ONLY the JSON (no prose, no markdown fence). Keep arrays focused (\u{2264}6 items). Ground each point in the evidence; do not invent functions or CVEs.

=== ATLAS SIGNALS ===
{{signals_md}}

=== SOURCE (path — possibly truncated) ===
{{source}}

Return JSON only as exactly: {\"snapshot_json\":\"<the JSON object above, as a JSON-encoded string>\"}";

fn prompt_output_fields() -> Value {
    json!([{ "field_name": "snapshot_json", "field_type": "string", "required": true, "min_length": 20 }])
}

// JS ensurePrompt: `existing = promptFind(...)`, best-effort (any error → null); if found AND
// its template_md matches ours byte-for-byte, no-op; otherwise (missing OR stale) promptAdd —
// note the JS never re-checks after promptAdd's own success/failure (fire-and-forget, matching
// pl-research.rs/book-run.rs's own ensure_* precedent of not propagating a promptAdd failure
// beyond the function boundary... except here JS doesn't wrap promptAdd in try/catch at all, so
// a real promptAdd failure DOES throw out of ensurePrompt/atlasFileSnapshot).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(Value::as_str) == Some(TEMPLATE_MD) {
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
            "max_output_tokens": 1600,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    Ok(())
}

// `pick(r) = r?.output_kv?.find(o => o.field_name === 'snapshot_json' && o.parent_output_id ==
// null)?.value_text ?? null` — pure, unreachable from atlasFileSnapshot (aiCall's Err
// short-circuits first) but unit-tested directly per the playbook.
fn pick(r: &Value) -> Value {
    let rows = match r.get("output_kv").and_then(Value::as_array) {
        Some(a) => a,
        None => return Value::Null,
    };
    for row in rows {
        let field_name = row.get("field_name").and_then(Value::as_str);
        // `o.parent_output_id == null` — JS loose equality matches BOTH null and undefined (an
        // absent key or an explicit null field both pass).
        let parent_is_null = row.get("parent_output_id").map_or(true, |v| v.is_null());
        if field_name == Some("snapshot_json") && parent_is_null {
            return nz(row.get("value_text")).cloned().unwrap_or(Value::Null);
        }
    }
    Value::Null
}

fn arr_trunc(v: Option<&Value>, n: usize) -> Vec<Value> {
    match v {
        Some(Value::Array(a)) => a.iter().filter(|x| !x.is_null()).take(n).cloned().collect(),
        _ => Vec::new(),
    }
}
fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
fn sev(v: Option<&Value>) -> String {
    let u = str_qq(v).to_lowercase();
    if ["critical", "high", "medium", "low"].contains(&u.as_str()) { u } else { "low".to_string() }
}

// `normalize(obj)` — pure, unreachable from atlasFileSnapshot for the same reason as `pick`
// (unit-tested directly).
fn normalize(obj: &Value) -> Value {
    let description = clip(&str_qq(obj.get("description")), 600);
    // `Math.max(0, Math.min(100, Math.round(Number(obj?.health ?? 50)) || 0))`
    let health_input = nz(obj.get("health")).cloned().unwrap_or(json!(50));
    let health_n = js_number(Some(&health_input));
    let rounded = crate::js_math::js_round(health_n); // Math.round
    let rounded = if rounded == 0.0 || rounded.is_nan() { 0.0 } else { rounded }; // `|| 0`
    let health = (rounded.max(0.0).min(100.0)) as i64;

    let complexity_raw = str_qq(obj.get("complexity"));
    let complexity = if ["low", "medium", "high"].contains(&complexity_raw.as_str()) { complexity_raw } else { "medium".to_string() };

    let responsibilities: Vec<Value> = arr_trunc(obj.get("responsibilities"), 12)
        .into_iter().map(|x| json!(clip(&str_qq(Some(&x)), 240))).collect();
    let insights: Vec<Value> = arr_trunc(obj.get("insights"), 12)
        .into_iter().map(|x| json!(clip(&str_qq(Some(&x)), 300))).collect();
    let vulnerabilities: Vec<Value> = arr_trunc(obj.get("vulnerabilities"), 12)
        .into_iter()
        .map(|v| json!({
            "severity": sev(v.get("severity")),
            "issue": clip(&str_qq(v.get("issue")), 300),
            "where": clip(&str_qq(v.get("where")), 120),
        }))
        .collect();
    let smells: Vec<Value> = arr_trunc(obj.get("smells"), 12)
        .into_iter().map(|x| json!(clip(&str_qq(Some(&x)), 240))).collect();
    let suggestions: Vec<Value> = arr_trunc(obj.get("suggestions"), 12)
        .into_iter().map(|x| json!(clip(&str_qq(Some(&x)), 300))).collect();

    json!({
        "description": description,
        "health": health,
        "complexity": complexity,
        "responsibilities": responsibilities,
        "insights": insights,
        "vulnerabilities": vulnerabilities,
        "smells": smells,
        "suggestions": suggestions,
    })
}

fn err_out(path: &Value, msg: String) -> Value {
    json!({ "ok": false, "path": path, "error": msg })
}

// `if (raw && String(raw).trim()) { let txt = String(raw).trim().replace(/^```(?:json)?/i, '').
// replace(/```$/i, '').trim(); parsed = JSON.parse(txt); }` — pure, unreachable from
// atlasFileSnapshot itself (aiCall's `Err` short-circuits before this ever runs, same as
// `pick`/`normalize`) but unit-tested directly per the playbook. `Ok(None)` = raw was
// falsy/blank (JS leaves `parsed` at its initial `null`); `Err` = JSON.parse threw.
fn strip_and_parse(raw: &Value) -> Result<Option<Value>, String> {
    let raw_s = str_qq(Some(raw));
    if !truthy(Some(raw)) || raw_s.trim().is_empty() {
        return Ok(None);
    }
    let trimmed = raw_s.trim();
    // `/^```(?:json)?/i` — case-insensitive leading fence, optional "json" tag.
    let lower = trimmed.to_lowercase();
    let after_fence = if lower.starts_with("```") {
        let rest = &trimmed[3..];
        let rest_lower = &lower[3..];
        if rest_lower.starts_with("json") { &rest[4..] } else { rest }
    } else {
        trimmed
    };
    // `/```$/i` — trailing fence (no letters, case irrelevant).
    let stripped = after_fence.strip_suffix("```").unwrap_or(after_fence).trim();
    serde_json::from_str::<Value>(stripped).map(Some).map_err(|e| e.to_string())
}

pub fn atlasFileSnapshot(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw
    // err.contextMissing('atlas-file-snapshot', 'db');` — unreachable here, `db` is a required
    // reference in every db-lane call site (same convention as every sibling twin).
    let origin = nz(input.get("origin")).cloned().unwrap_or(json!("atlas"));
    let origin_s = origin.as_str().unwrap_or("atlas").to_string();
    let path = input.get("path").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&path)) {
        return Err("atlas-file-snapshot: path is required".into());
    }
    let path_s = path.as_str().unwrap_or_default().to_string();

    // read the source (workspace-rooted, bounded) — the file may be off-atlas; that's fine.
    let mut read_input = Map::new();
    read_input.insert("path".into(), path.clone());
    if let Some(r) = input.get("root") { read_input.insert("root".into(), r.clone()); }
    read_input.insert("max_bytes".into(), json!(60 * 1024));
    let file = match crate::codebase_codebase_file_read::codebaseFileRead(&Value::Object(read_input), db) {
        Ok(f) => f,
        Err(e) => return Ok(err_out(&path, format!("cannot read source: {}", clip(&e, 160)))),
    };

    // gather atlas signals for this file (best-effort — an off-atlas file still gets analyzed on
    // source).
    let file_node_rows = crate::db_json::query_rows(db, "SELECT content_hash, intent FROM code.code_nodes WHERE origin=? AND path=? AND type='file' LIMIT 1", &[origin.clone(), path.clone()])?;
    let file_node = file_node_rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Object(Map::new()));

    let defs_rows = crate::db_json::query_rows(db, "SELECT type, name, signature FROM code.code_nodes WHERE origin=? AND path=? AND type!='file' ORDER BY type, name", &[origin.clone(), path.clone()])?;
    let defs = defs_rows.as_array().cloned().unwrap_or_default();

    let metrics: Option<Value> = crate::codebase_atlas_metrics::atlasMetrics(&json!({ "origin": origin, "top": 1 }), db)
        .ok()
        .and_then(|r| r.get("metrics_by_file").and_then(|m| m.get(&path_s)).cloned())
        .filter(|v| !v.is_null());
    let risk: Option<Value> = crate::codebase_atlas_score::atlasScore(&json!({ "origin": origin, "top": 1 }), db)
        .ok()
        .and_then(|r| r.get("by_file").and_then(|m| m.get(&path_s)).cloned())
        .filter(|v| !v.is_null());
    let smells: Vec<Value> = crate::codebase_atlas_smells::atlasSmells(&json!({ "origin": origin }), db)
        .ok()
        .and_then(|r| r.get("smells").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|s| {
            s.get("nodes").and_then(Value::as_array).map_or(false, |nodes| {
                nodes.iter().any(|n| str_qq(Some(n)).contains(&path_s))
            })
        })
        .take(6)
        .collect();

    let mut lines: Vec<String> = Vec::new();
    if let Some(intent) = file_node.get("intent").and_then(Value::as_str) {
        if truthy(Some(&json!(intent))) {
            lines.push(format!("Intent (atlas): {intent}"));
        }
    }
    lines.push(match &metrics {
        Some(m) => {
            let fan_in = m.get("fan_in").cloned().unwrap_or(Value::Null);
            let fan_out = m.get("fan_out").cloned().unwrap_or(Value::Null);
            let loc = m.get("loc").cloned().unwrap_or(Value::Null);
            let symbol_count = m.get("symbol_count").cloned().unwrap_or(Value::Null);
            let covered = truthy(m.get("covered"));
            let mut s = format!(
                "Metrics: {} importers \u{b7} {} imports \u{b7} {} loc \u{b7} {} symbols \u{b7} {}",
                str_qq(Some(&fan_in)), str_qq(Some(&fan_out)), str_qq(Some(&loc)), str_qq(Some(&symbol_count)),
                if covered { "covered" } else { "UNCOVERED" }
            );
            if let Some(cx) = m.get("complexity") {
                if !cx.is_null() {
                    s.push_str(&format!(" \u{b7} complexity {}", str_qq(Some(cx))));
                }
            }
            s
        }
        None => "Metrics: (file not in the atlas)".to_string(),
    });
    if let Some(r) = &risk {
        let risk_v = r.get("risk").cloned().unwrap_or(Value::Null);
        let mut factors: Vec<(String, f64)> = r.get("factors").and_then(Value::as_object)
            .map(|o| o.iter().filter_map(|(k, v)| {
                let n = js_number(Some(v));
                if n > 0.0 { Some((k.clone(), n)) } else { None }
            }).collect())
            .unwrap_or_default();
        factors.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let dominant = if factors.is_empty() {
            "\u{2014}".to_string()
        } else {
            factors.into_iter().take(2).map(|(k, _)| k).collect::<Vec<_>>().join(", ")
        };
        lines.push(format!("Risk score: {}/100 (dominant: {})", str_qq(Some(&risk_v)), dominant));
    }
    if !defs.is_empty() {
        let names: Vec<String> = defs.iter().take(24).map(|d| {
            let t = str_qq(d.get("type"));
            let n = str_qq(d.get("name"));
            format!("{t} {n}").trim().to_string()
        }).collect();
        lines.push(format!("Defs ({}): {}", defs.len(), names.join(", ")));
    }
    if !smells.is_empty() {
        let names: Vec<String> = smells.iter().map(|s| format!("{} ({})", str_qq(s.get("kind")), str_qq(s.get("severity")))).collect();
        lines.push(format!("Smells touching this file: {}", names.join(", ")));
    }
    if truthy(file.get("truncated")) {
        lines.push("(source truncated to 60KB for analysis)".to_string());
    }
    let signals_md = lines.join("\n");

    ensure_prompt(db)?;

    let source_full = str_qq(file.get("content"));
    let source_clipped: String = source_full.chars().take(48 * 1024).collect();

    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "signals_md", "value": signals_md },
            { "key": "source", "value": source_clipped },
        ],
        "tier": "mid",
        "temperature": "cold",
        "phase_flow": "single_only",
        "provider_fn": Value::Null,
    });
    // PARITY-DEFERRED boundary — the real JS wraps this + the JSON.parse in one try/catch and
    // returns `{ ok:false, path, error: 'analysis failed: ...' }` on ANY failure (network, parse,
    // etc). `crate::ai_ai_call::aiCall` always returns `Err`, so that branch is the only one
    // reachable here.
    let ai_result = match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(r) => r,
        Err(e) => return Ok(err_out(&path, format!("analysis failed: {}", clip(&e, 160)))),
    };

    let raw = pick(&ai_result);
    let parsed = match strip_and_parse(&raw) {
        Ok(v) => v,
        Err(e) => return Ok(err_out(&path, format!("analysis failed: {}", clip(&e, 160)))),
    };
    // `if (!parsed || typeof parsed !== 'object') return {...}` — JS `typeof` treats an ARRAY as
    // 'object' too, so a top-level JSON array would pass this gate (and normalize() would then
    // read every field off it as `undefined`, producing an all-defaults snapshot).
    let parsed = match parsed {
        Some(v @ Value::Object(_)) | Some(v @ Value::Array(_)) => v,
        _ => return Ok(err_out(&path, "AI returned no usable JSON snapshot".to_string())),
    };

    let snapshot = normalize(&parsed);
    let model_id = ai_result.get("model_id").cloned().unwrap_or(Value::Null);
    let content_hash = nz(file_node.get("content_hash")).cloned().unwrap_or_else(|| file.get("sha256").cloned().unwrap_or(Value::Null));

    let snapshot_id: Value = match db.execute(
        "INSERT INTO code.code_file_snapshots (origin, path, content_hash, snapshot_json, model_id) VALUES (?, ?, ?, ?, ?)",
        rusqlite::params![
            origin_s,
            path_s,
            content_hash.as_str(),
            serde_json::to_string(&snapshot).unwrap_or_default(),
            model_id.as_str(),
        ],
    ) {
        Ok(_) => json!(db.last_insert_rowid()),
        Err(_) => Value::Null, // table absent / read-only → still return the live snapshot
    };

    Ok(json!({ "ok": true, "path": path, "snapshot": snapshot, "snapshot_id": snapshot_id, "model_id": model_id }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_schema(c: &Connection) {
        c.execute_batch(
            "ATTACH ':memory:' AS code; ATTACH ':memory:' AS brain; \
             CREATE TABLE workspaces (root_path TEXT, is_active INTEGER); \
             CREATE TABLE code.code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, parent_id TEXT, span_json TEXT, content_hash TEXT, intent TEXT); \
             CREATE TABLE code.code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT); \
             CREATE TABLE code.code_signals (origin TEXT, category TEXT, kind TEXT, severity TEXT, path TEXT, line INTEGER, detail TEXT); \
             CREATE TABLE code.code_file_snapshots ( id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, path TEXT NOT NULL, content_hash TEXT, snapshot_json TEXT NOT NULL, model_id TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')) ); \
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) ); \
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
    }

    fn workspace_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("atlas-file-snapshot-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_path_errs() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        assert!(atlasFileSnapshot(&json!({}), &c).is_err());
    }

    #[test]
    fn cannot_read_source_returns_ok_false() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let dir = workspace_dir();
        let r = atlasFileSnapshot(&json!({ "path": "does/not/exist.ts", "root": dir.to_string_lossy() }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("cannot read source"));
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_after_full_deterministic_surface() {
        // proves the guard/source-read/signal-gather/signals_md/ensure_prompt surface all ran
        // cleanly and the aiCall failure is surfaced as ok:false (not an earlier bug).
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let dir = workspace_dir();
        std::fs::write(dir.join("a.ts"), "export function f() {}\n").unwrap();
        c.execute("INSERT INTO code.code_nodes (id,origin,path,type,name,signature,parent_id,span_json,content_hash,intent) VALUES ('a.ts@atlas','atlas','a.ts','file','a.ts',NULL,NULL,'{\"end_line\":1}','h1','does a thing')", []).unwrap();
        c.execute("INSERT INTO code.code_nodes (id,origin,path,type,name,signature,parent_id,span_json,content_hash,intent) VALUES ('a.ts#f','atlas','a.ts','function','f','()=>void', 'a.ts@atlas',NULL,NULL,NULL)", []).unwrap();
        let r = atlasFileSnapshot(&json!({ "path": "a.ts", "root": dir.to_string_lossy() }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        let msg = r["error"].as_str().unwrap();
        assert!(msg.starts_with("analysis failed:"), "unexpected error: {msg}");
        assert!(msg.contains("ai-call"), "unexpected error: {msg}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'atlas.file.snapshot.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    // ── pure helpers, unreachable from atlasFileSnapshot (aiCall's Err short-circuits first) ──

    #[test]
    fn pick_finds_top_level_snapshot_json_only() {
        let r = json!({ "output_kv": [
            { "field_name": "snapshot_json", "value_text": "{\"a\":1}", "parent_output_id": null },
        ]});
        assert_eq!(pick(&r), json!("{\"a\":1}"));
        let r2 = json!({ "output_kv": [
            { "field_name": "snapshot_json", "value_text": "nested", "parent_output_id": 4 },
        ]});
        assert_eq!(pick(&r2), Value::Null);
        assert_eq!(pick(&json!({})), Value::Null);
    }

    #[test]
    fn normalize_coerces_every_key_present_and_clamps_health() {
        let out = normalize(&json!({
            "description": "d", "health": 250, "complexity": "bogus",
            "responsibilities": ["r1", null, "r2"],
            "vulnerabilities": [{ "severity": "EXTREME", "issue": "i", "where": "w" }],
        }));
        assert_eq!(out["health"], json!(100));
        assert_eq!(out["complexity"], json!("medium"));
        assert_eq!(out["responsibilities"], json!(["r1", "r2"]));
        assert_eq!(out["vulnerabilities"][0]["severity"], json!("low"));
        assert_eq!(out["insights"], json!([]));
        assert_eq!(out["smells"], json!([]));
        assert_eq!(out["suggestions"], json!([]));
    }

    #[test]
    fn strip_and_parse_handles_fences_blank_and_bad_json() {
        assert_eq!(strip_and_parse(&json!("  ```json\n{\"a\":1}\n```  ")).unwrap(), Some(json!({ "a": 1 })));
        assert_eq!(strip_and_parse(&json!("```\n[1,2]\n```")).unwrap(), Some(json!([1, 2])));
        assert_eq!(strip_and_parse(&json!("  \n  ")).unwrap(), None);
        assert_eq!(strip_and_parse(&Value::Null).unwrap(), None);
        assert!(strip_and_parse(&json!("not json")).is_err());
    }

    #[test]
    fn normalize_negative_and_missing_health_defaults() {
        let out = normalize(&json!({ "health": -10 }));
        assert_eq!(out["health"], json!(0));
        let out2 = normalize(&json!({}));
        assert_eq!(out2["health"], json!(50));
        assert_eq!(out2["description"], json!(""));
    }
}
