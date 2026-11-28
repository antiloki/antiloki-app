//! constitution-insights.rs — twin of skills/constitution/constitution-insights.mjs (db lane,
//! cross-db `laws`+`judgments`+`brain`). NAMES ARE CONTRACT: default `constitutionInsights`.
//! THE AI PHASE (docs/constitution-review-plan.html N1-N3): N1 picks the snapshot's worst files
//! (risk-ranked, capped by `max_spots`); N2 runs ONE low-tier AI call per spot to synthesize an
//! insight from the ALREADY-computed broken laws + weak engineering axes; N3 escalates a 2nd
//! lens only for extreme-risk spots or a first output that failed to parse.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`, so `run_spot`
//! propagates that error the instant it is called, exactly mirroring the JS's
//! `await aiCall(...)` throw point. Because of that, `constitutionInsights` itself is only
//! fixture-reachable up to (and including) the no-spots early return — every spot with a real
//! row hits the outer per-spot `try/catch` (mirrored here as a per-spot `Result`) and always
//! lands on `dropped += 1`, deterministically, the same way the JS would if its own `aiCall`
//! import were swapped for an always-rejecting stub. The deterministic surface ABOVE that
//! boundary — guard, root/snapshot resolution, spot selection, violation-text/axes-text
//! rendering, `ensure_prompt`, and the escalation/insert bookkeeping shape — is ported faithfully
//! and unit-tested directly (`violations_text`/`axes_text`/`extract_json`/`ensure_prompt`), same
//! convention as pl-research.rs's response-shaping helpers.
//!
//! `resolve(input.root)` (single-arg `node:path.resolve`, JS: relative inputs anchor against
//! `process.cwd()`): this twin follows workspace-root.rs's own disclosed convention for
//! single-segment `resolve()` — every real call site passes an already-absolute root, so a bare
//! `win_normalize` stands in; a genuinely relative `input.root` (which would pull in the
//! process's cwd, an environment-dependent value with no DB-derived fixture anchor) is out of
//! scope here, same as workspace-root.rs's `win_resolve1`.
//!
//! `context.db` missing → JS throws `err.required('constitution-insights', 'context.db')`. Not
//! modeled: `db: &Connection` is a required, non-nullable reference (same convention as every
//! sibling twin). `context.provider_fn` (a JS function reference) has no Rust equivalent to
//! carry — threaded as `null`, same as pl-research.rs/book-run.rs's `provider_fn`.
// QUALITY: mixed unit — deterministic assembly/db-read/insert surface (fully ported, fixture-
// reachable up to the deferred aiCall boundary) + 3 pure text-rendering helpers (independently
// unit-tested) + 1 bespoke/deferred aiCall dispatch (no fixture/frontier).

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
} // = JS `${v}`

const PROMPT: &str = "constitution-insight";
const SYSTEM: &str = "You are a staff engineer writing the fix note for ONE code spot that DETERMINISTIC analysis has ALREADY convicted — you are told which house LAWS it breaks (if any) AND which engineering AXES are weak (structure/coupling/tests/change/knowledge, each 0-100 with its counted parts). Do NOT hunt for new problems (the detectors already ran; inventing more manufactures noise). SYNTHESIZE: explain in plain terms why THIS spot is weak — tie it to the specific broken laws and/or the weak axes — and give the concrete first fix. Rules: (1) address ONLY what you were given (cite each law_key; name the weak axes); (2) ground every statement in the given data + source — never invent APIs or behavior; (3) the fix is the smallest change that raises the weak signal; (4) classify: correct (a real defect) | good (quality) | consistent (house-convention drift) | complete (something missing). Output ONLY valid JSON: {\"insight\":\"2-4 sentences tying the broken laws/weak axes to why it matters here\",\"fix_sketch\":\"the concrete first move, 2-4 sentences\",\"kind\":\"correct|good|consistent|complete\",\"cited_laws\":[\"law-key\"]}. No prose outside the JSON.";
const TEMPLATE: &str = "SPOT: {{path}} ({{score}})\n\nBROKEN LAWS (key · weight · what it requires · offending lines):\n{{laws}}\n\nWEAK ENGINEERING AXES (score · parts):\n{{axes}}\n\nSOURCE (may be truncated):\n{{source}}\n\nOutput the fix-note JSON.";

const MAX_SOURCE: usize = 4500;

// ── win32 path port (drive-letter roots only — same disclosed scope as workspace-root.rs) ────
fn win_is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    if b.is_empty() { return false; }
    if b[0] == b'/' || b[0] == b'\\' { return true; }
    if b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\') { return true; }
    false
}
fn abs_path(root: &str, p: &str) -> PathBuf {
    if win_is_absolute(p) { PathBuf::from(p) } else { Path::new(root).join(p) }
}

/// `function extractJson(text)` — first `{` .. last `}` slice, `JSON.parse`'d; any failure (no
/// braces, malformed JSON) → `null`.
fn extract_json(text: &str) -> Option<Value> {
    let a = text.find('{')?;
    let b = text.rfind('}')?;
    if b <= a { return None; }
    serde_json::from_str(&text[a..=b]).ok()
}

/// `function ensurePrompt(db)` — reuses the already-converted prompt-find/prompt-add twins.
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(Value::as_str) == Some(TEMPLATE)
            && e.get("system_md").and_then(Value::as_str) == Some(SYSTEM)
        {
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
            "max_output_tokens": 700,
            "output_fields": [{ "field_name": "insight_json", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

/// the BROKEN LAWS block for one spot — joined `law_key · w<weight> · <rule_md|title> ·
/// [line N ·] «evidence»` lines, or the "no law violations" fallback when the spot has none.
fn violations_text(db: &Connection, snap_id: i64, path: &str, law_by_key: &HashMap<String, Value>) -> Result<String, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT law_key, weight, line, evidence FROM judgments.review_violations WHERE snapshot_id=? AND path=? ORDER BY weight DESC LIMIT 12",
        &[json!(snap_id), json!(path)],
    )?;
    let arr = rows.as_array().cloned().unwrap_or_default();
    if arr.is_empty() {
        return Ok("(no law violations — this spot is weak by engineering health, not law-breaking)".to_string());
    }
    let lines: Vec<String> = arr
        .iter()
        .map(|v| {
            let law_key = tmpl(v.get("law_key"));
            let weight = tmpl(v.get("weight"));
            let law = law_by_key.get(&law_key);
            let rule = law
                .and_then(|l| l.get("rule_md")).and_then(Value::as_str)
                .or_else(|| law.and_then(|l| l.get("title")).and_then(Value::as_str))
                .unwrap_or("");
            let line_part = if v.get("line").map_or(false, |l| !l.is_null()) {
                format!(" · line {}", tmpl(v.get("line")))
            } else {
                String::new()
            };
            format!("{law_key} · w{weight} · {rule}{line_part} · «{}»", tmpl(v.get("evidence")))
        })
        .collect();
    Ok(lines.join("\n"))
}

/// the WEAK ENGINEERING AXES block — `axes_json` entries with `score < 60`, ascending by score,
/// rendered `key=score (part:val part:val)`; `(axes healthy)` when none are weak; `(no axes)` on
/// malformed/missing JSON.
fn axes_text(spot: &Value) -> String {
    let ax_str = spot.get("axes_json").and_then(Value::as_str).unwrap_or("{}");
    let ax: Value = match serde_json::from_str(ax_str) {
        Ok(v) => v,
        Err(_) => return "(no axes)".to_string(),
    };
    let obj = ax.as_object().cloned().unwrap_or_default(); // JSON.parse(...) ?? {} — null collapses to {}
    let mut weak: Vec<(String, Value)> = obj
        .into_iter()
        .filter(|(_, a)| a.get("score").and_then(Value::as_f64).map_or(false, |s| s < 60.0))
        .collect();
    weak.sort_by(|a, b| {
        let as_ = a.1.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let bs = b.1.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        as_.partial_cmp(&bs).unwrap_or(std::cmp::Ordering::Equal)
    });
    if weak.is_empty() {
        return "(axes healthy)".to_string();
    }
    weak.iter()
        .map(|(k, a)| {
            let score = tmpl(a.get("score"));
            let parts = a
                .get("parts")
                .and_then(Value::as_object)
                .map(|p| p.iter().map(|(pk, pv)| format!("{pk}:{}", tmpl(Some(pv)))).collect::<Vec<_>>().join(" "))
                .unwrap_or_default();
            format!("{k}={score} ({parts})")
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// one AI-call result for one spot/attempt.
struct SpotResult {
    body: Option<Value>,
    model: Value,
    cost: f64,
}

fn has_insight(r: &SpotResult) -> bool {
    match &r.body {
        Some(b) => truthy(b.get("insight")),
        None => false,
    }
}

/// `const runSpot = async (spot, attempt) => {...}` — reads the source (truncated to
/// `MAX_SOURCE` chars), renders the broken-laws + weak-axes blocks, dispatches the AI call
/// (PARITY-DEFERRED — see module doc), and parses the `insight_json` output field.
fn run_spot(
    db: &Connection,
    root: &str,
    spot: &Value,
    snap_id: i64,
    law_by_key: &HashMap<String, Value>,
    attempt: i64,
    provider_fn: &Value,
) -> Result<SpotResult, String> {
    let path = spot.get("path").and_then(Value::as_str).unwrap_or("");
    let source: String = match std::fs::read_to_string(abs_path(root, path)) {
        Ok(s) => s.chars().take(MAX_SOURCE).collect(),
        Err(_) => "(unreadable)".to_string(),
    };
    let laws_text = violations_text(db, snap_id, path, law_by_key)?;
    let axes = axes_text(spot);
    let health = match spot.get("health") { None | Some(Value::Null) => "?".to_string(), Some(v) => tmpl(Some(v)) };
    let score_str = format!("health {health} · law-compliance {}", tmpl(spot.get("score")));

    let mut input_kv = vec![
        json!({ "key": "path", "value": path }),
        json!({ "key": "score", "value": score_str }),
        json!({ "key": "axes", "value": axes }),
        json!({ "key": "laws", "value": laws_text }),
        json!({ "key": "source", "value": source }),
    ];
    if attempt > 1 {
        input_kv.push(json!({ "key": "_retry", "value": attempt.to_string() }));
    }

    let ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": input_kv,
        "bypass_cache": attempt > 1,
        "provider_fn": provider_fn.clone(),
    });
    let res = crate::ai_ai_call::aiCall(&ai_input, db)?;

    let cost = res.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
    let text = res
        .get("output_kv")
        .and_then(Value::as_array)
        .and_then(|arr| {
            arr.iter().find(|o| {
                o.get("field_name").and_then(Value::as_str) == Some("insight_json")
                    && o.get("parent_output_id").map_or(true, |v| v.is_null())
            })
        })
        .and_then(|o| o.get("value_text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let model = res.get("model_id").cloned().unwrap_or(Value::Null);
    Ok(SpotResult { body: extract_json(&text), model, cost })
}

fn js_is_integer(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::Number(n)) if n.as_f64().map_or(false, |f| f.is_finite() && f.fract() == 0.0))
}

/// `export default async function constitutionInsights(input, context)`
pub fn constitutionInsights(input: &Value, db: &Connection) -> Result<Value, String> {
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);

    let root: String = match input.get("root").filter(|v| truthy(Some(v))) {
        // single-arg `resolve()` — see module doc: real call sites pass an already-absolute root.
        Some(v) => tmpl(Some(v)),
        None => crate::engine_workspace_root::getWorkspaceRoot(db).as_str().unwrap_or_default().to_string(),
    };

    let snap: Option<Value> = if input.get("snapshot_id").map_or(false, |v| !v.is_null()) {
        let id = input.get("snapshot_id").and_then(Value::as_f64).unwrap_or(0.0) as i64;
        crate::db_json::query_rows(db, "SELECT * FROM judgments.review_snapshots WHERE id=?", &[json!(id)])?
            .as_array()
            .and_then(|a| a.first())
            .cloned()
    } else {
        crate::db_json::query_rows(db, "SELECT * FROM judgments.review_snapshots WHERE status='done' ORDER BY id DESC LIMIT 1", &[])?
            .as_array()
            .and_then(|a| a.first())
            .cloned()
    };
    let snap = match snap {
        Some(s) => s,
        None => return Err("constitution-insights: a done snapshot required".to_string()),
    };
    let snap_id = snap.get("id").and_then(Value::as_i64).unwrap_or(0);

    let max_spots: i64 = {
        let raw = if js_is_integer(input.get("max_spots")) { input.get("max_spots").and_then(Value::as_i64).unwrap_or(12) } else { 12 };
        raw.max(1).min(60)
    };
    let escalate_top: i64 = {
        let raw = if js_is_integer(input.get("escalate_top")) { input.get("escalate_top").and_then(Value::as_i64).unwrap_or(3) } else { 3 };
        raw.max(0)
    };

    let composed = crate::constitution_constitution_law::composedConstitution(db);
    let laws: Vec<Value> = composed.get("laws").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut law_by_key: HashMap<String, Value> = HashMap::new();
    for l in &laws {
        if let Some(k) = l.get("key").and_then(Value::as_str) {
            law_by_key.insert(k.to_string(), l.clone());
        }
    }
    ensure_prompt(db)?;

    let spots: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT path, score, health, risk, breakdown_json, axes_json FROM judgments.review_scores WHERE snapshot_id=? AND unit_kind='file' AND (health < 92 OR score < 100) ORDER BY risk DESC LIMIT ?",
        &[json!(snap_id), json!(max_spots)],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();

    if spots.is_empty() {
        return Ok(json!({
            "snapshot_id": snap_id, "spots": 0, "made": 0, "dropped": 0, "escalated": 0,
            "cost_usd": 0, "insights": {},
        }));
    }

    let provider_fn = Value::Null; // no Rust equivalent for a JS function reference — see module doc

    let mut made: i64 = 0;
    let mut dropped: i64 = 0;
    let mut escalated: i64 = 0;
    let mut cost: f64 = 0.0;

    for (rank, spot) in spots.iter().enumerate() {
        let path = spot.get("path").and_then(Value::as_str).unwrap_or("").to_string();
        let outcome: Result<(), String> = (|| {
            let r1 = run_spot(db, &root, spot, snap_id, &law_by_key, 1, &provider_fn)?;
            cost += r1.cost;
            let mut r = r1;
            let mut attempts: i64 = 1;
            let extreme = (rank as i64) < escalate_top;
            if !has_insight(&r) || extreme {
                let r2 = run_spot(db, &root, spot, snap_id, &law_by_key, 2, &provider_fn)?;
                cost += r2.cost;
                attempts = 2;
                escalated += 1;
                if has_insight(&r2) { r = r2; }
            }
            if has_insight(&r) {
                let body_json: String = serde_json::to_string(r.body.as_ref().unwrap()).unwrap_or_default().chars().take(6000).collect();
                crate::db_json::run_write(
                    db,
                    "INSERT INTO judgments.review_insights (snapshot_id, path, body_json, model, attempts, cost_usd) VALUES (?, ?, ?, ?, ?, ?)",
                    &[json!(snap_id), json!(path), json!(body_json), r.model.clone(), json!(attempts), json!(0)],
                )?;
                made += 1;
            } else {
                dropped += 1;
            }
            Ok(())
        })();
        if outcome.is_err() {
            dropped += 1;
        }
    }

    crate::db_json::run_write(
        db,
        "UPDATE judgments.review_snapshots SET totals_json=json_set(coalesce(totals_json,'{}'), '$.ai_cost_usd', ?) WHERE id=?",
        &[json!((cost * 10000.0).round() / 10000.0), json!(snap_id)],
    )?;

    let insight_rows: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT path, body_json, model, attempts FROM judgments.review_insights WHERE snapshot_id=?",
        &[json!(snap_id)],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();
    let mut insights = Map::new();
    for r in &insight_rows {
        let body_json = r.get("body_json").and_then(Value::as_str).unwrap_or("");
        if let Some(b) = extract_json(body_json) {
            let mut merged = b.as_object().cloned().unwrap_or_default();
            merged.insert("model".to_string(), r.get("model").cloned().unwrap_or(Value::Null));
            merged.insert("attempts".to_string(), r.get("attempts").cloned().unwrap_or(Value::Null));
            let path = r.get("path").and_then(Value::as_str).unwrap_or("").to_string();
            insights.insert(path, Value::Object(merged));
        }
    }

    Ok(json!({
        "snapshot_id": snap_id,
        "spots": spots.len() as i64,
        "made": made,
        "dropped": dropped,
        "escalated": escalated,
        "cost_usd": (cost * 10000.0).round() / 10000.0,
        "insights": Value::Object(insights),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn extract_json_variants() {
        assert_eq!(extract_json(r#"noise {"insight":"x"} trailer"#), Some(json!({"insight":"x"})));
        assert_eq!(extract_json("no braces here"), None);
        assert_eq!(extract_json("{ malformed"), None);
        assert_eq!(extract_json("{}{"), Some(json!({}))); // first '{' .. last '}' still valid JSON
    }

    #[test]
    fn violations_text_fallback_and_rendering() {
        let c = db();
        crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        let by_key: HashMap<String, Value> = HashMap::new();
        assert_eq!(
            violations_text(&c, 1, "src/a.mjs", &by_key).unwrap(),
            "(no law violations — this spot is weak by engineering health, not law-breaking)"
        );
        c.execute(
            "INSERT INTO judgments.review_violations (snapshot_id, path, law_key, weight, line, evidence) VALUES (1, 'src/a.mjs', 'no-comments', 5, 12, 'bad code')",
            [],
        ).unwrap();
        let mut laws = HashMap::new();
        laws.insert("no-comments".to_string(), json!({ "rule_md": "avoid X" }));
        let out = violations_text(&c, 1, "src/a.mjs", &laws).unwrap();
        assert_eq!(out, "no-comments · w5 · avoid X · line 12 · «bad code»");
    }

    #[test]
    fn axes_text_variants() {
        assert_eq!(axes_text(&json!({ "axes_json": null })), "(axes healthy)");
        assert_eq!(axes_text(&json!({ "axes_json": "not json" })), "(no axes)");
        assert_eq!(axes_text(&json!({ "axes_json": "{\"structure\":{\"score\":90}}" })), "(axes healthy)");
        let ax = json!({ "structure": { "score": 40, "parts": { "loc": 900 } }, "tests": { "score": 20, "parts": {} } });
        let out = axes_text(&json!({ "axes_json": serde_json::to_string(&ax).unwrap() }));
        assert_eq!(out, "tests=20 () · structure=40 (loc:900)");
    }

    #[test]
    fn ensure_prompt_creates_then_is_idempotent() {
        let c = db();
        ensure_prompt(&c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name='constitution-insight'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        ensure_prompt(&c).unwrap(); // matching template/system → no second insert
        let n2: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name='constitution-insight'", [], |r| r.get(0)).unwrap();
        assert_eq!(n2, 1);
    }

    #[test]
    fn no_done_snapshot_errors() {
        let c = db();
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', 'C:\\tmp', 1)", []).unwrap();
        let e = constitutionInsights(&json!({}), &c).unwrap_err();
        assert_eq!(e, "constitution-insights: a done snapshot required");
    }

    #[test]
    fn no_qualifying_spots_returns_early_without_touching_ai() {
        let c = db();
        crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute("INSERT INTO judgments.review_snapshots (id, constitution_hash, status) VALUES (1, 'h', 'done')", []).unwrap();
        let r = constitutionInsights(&json!({}), &c).unwrap();
        assert_eq!(r["snapshot_id"], json!(1));
        assert_eq!(r["spots"], json!(0));
        assert_eq!(r["made"], json!(0));
        assert_eq!(r["dropped"], json!(0));
        assert_eq!(r["escalated"], json!(0));
        assert_eq!(r["insights"], json!({}));
    }

    #[test]
    fn qualifying_spots_all_drop_at_the_deferred_aicall_boundary() {
        let c = db();
        crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', 'C:\\tmp', 1)", []).unwrap();
        c.execute("INSERT INTO judgments.review_snapshots (id, constitution_hash, status) VALUES (1, 'h', 'done')", []).unwrap();
        c.execute("INSERT INTO judgments.review_scores (snapshot_id, path, unit_kind, health, score, risk, axes_json) VALUES (1, 'src/a.mjs', 'file', 40, 60, 5.0, '{}')", []).unwrap();
        c.execute("INSERT INTO judgments.review_scores (snapshot_id, path, unit_kind, health, score, risk, axes_json) VALUES (1, 'src/b.mjs', 'file', 95, 100, 1.0, NULL)", []).unwrap();
        let r = constitutionInsights(&json!({ "max_spots": 5, "escalate_top": 1 }), &c).unwrap();
        assert_eq!(r["snapshot_id"], json!(1));
        assert_eq!(r["spots"], json!(1)); // only the health<92-or-score<100 row qualifies
        assert_eq!(r["made"], json!(0));
        assert_eq!(r["dropped"], json!(1));
        assert_eq!(r["escalated"], json!(0)); // run_spot(attempt=1) errors before escalation logic runs
        assert_eq!(r["insights"], json!({}));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM judgments.review_insights", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn snapshot_id_input_selects_a_specific_snapshot() {
        let c = db();
        crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute("INSERT INTO judgments.review_snapshots (id, constitution_hash, status) VALUES (1, 'h1', 'done')", []).unwrap();
        c.execute("INSERT INTO judgments.review_snapshots (id, constitution_hash, status) VALUES (2, 'h2', 'done')", []).unwrap();
        let r = constitutionInsights(&json!({ "snapshot_id": 1 }), &c).unwrap();
        assert_eq!(r["snapshot_id"], json!(1));
    }
}
