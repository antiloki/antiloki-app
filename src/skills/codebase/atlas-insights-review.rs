//! atlas-insights-review.rs — twin of skills/codebase/atlas-insights-review.mjs (db READ lane +
//! deferred AI narrative). NAMES ARE CONTRACT: default `atlasInsightsReview`. The one NON-
//! deterministic Insights lens: a short, advisory project review GROUNDED in the deterministic
//! insights — the health/risk score (top risky files + factors, via the already-converted
//! `codebase_atlas_score` twin), the structural smells (`codebase_atlas_smells` twin), the
//! architecture spine (`codebase_architecture_digest::read_skeleton`), and — when passed —
//! dependency vulnerabilities. It asks a MID-tier model to synthesize a brief with a BOUNDED
//! evidence pack (never the whole graph). Advisory + non-gating; degrades gracefully on any
//! provider failure. Persists nothing.
//!
//! `context.db` missing → JS throws `err.contextMissing('atlas-insights-review', 'db')`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (same convention as
//! every sibling db-lane twin). `context.provider_fn` has no Rust equivalent — threaded as `null`
//! into the constructed aiCall input, same pattern as pl-research.rs/architecture-digest.rs.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`. Like
//! architecture-digest.rs (and UNLIKE pl-research/book-run), the JS wraps its aiCall in a
//! try/catch and returns a STATUS object (`{review_md:null, ok:false, error}`) instead of
//! throwing — this twin mirrors that exactly: `atlasInsightsReview` never returns `Err`, it
//! ALWAYS resolves the deferred aiCall failure into an `ok:false` status value, so the entire
//! deterministic surface (guard, evidence assembly, `ensure_review_prompt`) is fixture-reachable
//! end-to-end. `pick_review`/`build_evidence`/`top_factors` are ported as pure functions and unit-
//! tested directly with canned values.
// QUALITY: fully deterministic status-shaped surface (fixture-reachable end-to-end) + 1
// bespoke/deferred aiCall dispatch whose failure is caught, not propagated.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

const PROMPT: &str = "atlas.insights.review.v2";
const SYSTEM: &str = "You are a senior engineer writing ONE short ADVISORY project brief (non-blocking — you are NOT a gate). You are given a project's x-ray: its architecture SPINE (the most-imported files and how they depend), a health/risk score, the riskiest files with their risk factors, structural smells, and known dependency vulnerabilities. You output only strict JSON. Ground every point in the evidence given; invent nothing (no files, no CVEs, no metrics that are not listed).";
const TEMPLATE: &str = "Write ONE short project brief (markdown) from the x-ray below — it MERGES an architecture read and a health review into a single narrative. Cover, in order: (1) ARCHITECTURE — the entry points, layers, and main modules, and how they depend (read from the spine); (2) HEALTH — the headline: how healthy is this codebase and why; (3) FRAGILITY — what is most at risk right now; (4) FIX FIRST — the top 3 things to refactor or fix first, each with a one-line reason tied to the evidence. Advisory only — you are not a gate. Ground every point in the evidence; do not invent files, CVEs, or numbers that are not listed.\n\n=== PROJECT X-RAY (the evidence) ===\n{{evidence_md}}\n\nReturn JSON only as exactly: {\"review_md\":\"<short project brief in markdown>\"}";

fn prompt_output_fields() -> Value {
    json!([{ "field_name": "review_md", "field_type": "string", "required": true, "min_length": 40 }])
}

fn ensure_review_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(Value::as_str) == Some(TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT, "template_md": TEMPLATE, "system_md": SYSTEM,
            "default_tier": "mid", "default_temperature": "cold", "phase_flow": "single_only",
            "max_output_tokens": 1800, "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    Ok(())
}

// `r?.output_kv?.find(o => o.field_name === 'review_md' && o.parent_output_id == null)?.value_text ?? null`
fn pick_review(r: &Value) -> Option<String> {
    let rows = r.get("output_kv")?.as_array()?;
    let row = rows.iter().find(|o| {
        o.get("field_name").and_then(Value::as_str) == Some("review_md")
            && o.get("parent_output_id").map_or(true, |v| v.is_null())
    })?;
    row.get("value_text").and_then(Value::as_str).map(|s| s.to_string())
}

// The dominant risk factors of a file (normalized 0-1), highest first, as a compact hint.
// JS: `Object.entries(factors ?? {}).filter(([, v]) => v > 0).sort((a,b) => b[1]-a[1]).slice(0,2)
//      .map(([k,v]) => \`${k} ${v.toFixed(2)}\`).join(', ') || '—'`
fn top_factors(factors: Option<&Value>) -> String {
    let mut entries: Vec<(String, f64)> = match factors.and_then(Value::as_object) {
        Some(m) => m
            .iter()
            .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f)))
            .filter(|(_, v)| *v > 0.0)
            .collect(),
        None => Vec::new(),
    };
    entries.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    entries.truncate(2);
    if entries.is_empty() {
        return "—".to_string();
    }
    entries.iter().map(|(k, v)| format!("{k} {v:.2}")).collect::<Vec<_>>().join(", ")
}

// Bounded markdown evidence pack from the deterministic insights — never the whole graph.
fn build_evidence(score: &Value, smells: &Value, vulns: Option<&[Value]>, skeleton: &[String]) -> String {
    let mut lines: Vec<String> = Vec::new();

    if !skeleton.is_empty() {
        lines.push("Architecture spine (most-imported files → their in-scope imports; the load-bearing modules):".to_string());
        for l in skeleton {
            lines.push(format!("- {l}"));
        }
        lines.push(String::new());
    }

    let health = score.get("project").and_then(|p| p.get("health")).cloned().unwrap_or(json!(0));
    let risk = score.get("project").and_then(|p| p.get("risk")).cloned().unwrap_or(json!(0));
    let file_count = score.get("file_count").and_then(Value::as_i64).unwrap_or(0);
    let churn_available = score.get("churn_available").and_then(Value::as_bool).unwrap_or(false);
    lines.push(format!(
        "Project health: {}/100 · risk: {}/100 ({} source files{}).",
        health, risk, file_count, if churn_available { ", churn available" } else { "" }
    ));

    lines.push(String::new());
    lines.push("Riskiest files (risk/100 — dominant factors):".to_string());
    let top_risk: Vec<Value> = score.get("top_risk").and_then(Value::as_array).cloned().unwrap_or_default();
    if !top_risk.is_empty() {
        for f in top_risk.iter().take(8) {
            let risk_n = f.get("risk").cloned().unwrap_or(json!(0));
            let path = f.get("path").and_then(Value::as_str).unwrap_or("");
            lines.push(format!("- {} {} — {}", risk_n, path, top_factors(f.get("factors"))));
        }
    } else {
        lines.push("- (none)".to_string());
    }

    let smell_list: Vec<Value> = smells.get("smells").and_then(Value::as_array).cloned().unwrap_or_default();
    let counts: Vec<String> = smells
        .get("counts")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter(|(k, _)| k.as_str() != "total")
                .map(|(k, v)| format!("{k}:{v}"))
                .collect()
        })
        .unwrap_or_default();
    let counts_str = counts.join(", ");
    lines.push(String::new());
    lines.push(format!(
        "Structural smells ({} total{}):",
        smell_list.len(),
        if !counts_str.is_empty() { format!(" — {counts_str}") } else { String::new() }
    ));
    if !smell_list.is_empty() {
        for s in smell_list.iter().take(25) {
            let severity = s.get("severity").and_then(Value::as_str).unwrap_or("");
            let kind = s.get("kind").and_then(Value::as_str).unwrap_or("");
            let detail = s.get("detail").and_then(Value::as_str).unwrap_or("");
            lines.push(format!("- [{severity}] {kind}: {detail}"));
        }
    } else {
        lines.push("- (none)".to_string());
    }

    match vulns {
        Some(vs) => {
            let mut by_sev: Map<String, Value> = Map::new();
            for v in vs {
                let sev = v.get("severity").and_then(Value::as_str).unwrap_or("").to_string();
                let cur = by_sev.get(&sev).and_then(Value::as_i64).unwrap_or(0);
                by_sev.insert(sev, json!(cur + 1));
            }
            let sev_str = if by_sev.is_empty() {
                "none".to_string()
            } else {
                by_sev.iter().map(|(k, v)| format!("{k}:{v}")).collect::<Vec<_>>().join(", ")
            };
            lines.push(String::new());
            lines.push(format!("Dependency vulnerabilities ({} total — {}):", vs.len(), sev_str));
            for v in vs.iter().take(12) {
                let severity = v.get("severity").and_then(Value::as_str).unwrap_or("");
                let package = v.get("package").and_then(Value::as_str).unwrap_or("");
                let version = v.get("version").and_then(Value::as_str).unwrap_or("");
                let cve = v.get("cve").and_then(Value::as_str).unwrap_or("");
                let fixed_in = v.get("fixed_in").and_then(Value::as_str);
                let fixed = fixed_in.map(|f| format!(" → fixed in {f}")).unwrap_or_default();
                lines.push(format!("- [{severity}] {package}@{version} {cve}{fixed}"));
            }
        }
        None => {
            lines.push(String::new());
            lines.push("Dependency vulnerabilities: not scanned.".to_string());
        }
    }

    lines.join("\n")
}

pub fn atlasInsightsReview(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = nz(input.get("origin")).and_then(|v| v.as_str()).unwrap_or("atlas").to_string();

    let score_input = json!({
        "origin": &origin,
        "root": input.get("root").cloned().unwrap_or(Value::Null),
        "top": 8,
    });
    let score = crate::codebase_atlas_score::atlasScore(&score_input, db)?;
    let smells = crate::codebase_atlas_smells::atlasSmells(&json!({ "origin": &origin }), db)?;
    let (skeleton, _considered) = crate::codebase_architecture_digest::read_skeleton(db, &Value::String(origin.clone()), 40)?;
    let vulns: Option<Vec<Value>> = input.get("vulns").and_then(Value::as_array).cloned();

    let file_count = score.get("file_count").and_then(Value::as_i64).unwrap_or(0);
    let smells_len = smells.get("smells").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
    let grounded = json!({
        "files": file_count,
        "smells": smells_len as i64,
        "vulns": vulns.as_ref().map(|v| v.len() as i64),
        "spine": skeleton.len() as i64,
    });

    if file_count == 0 {
        return Ok(json!({
            "review_md": Value::Null,
            "ok": false,
            "error": format!("empty atlas for origin '{origin}' — nothing to review"),
            "grounded": grounded,
        }));
    }

    let evidence_md = build_evidence(&score, &smells, vulns.as_deref(), &skeleton);
    ensure_review_prompt(db)?;

    // ── the AI call: PARITY-DEFERRED (see module doc) — aiCall always returns Err; the JS
    // catches it here (unlike pl-research/book-run) and returns a status object, never throws.
    let ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [{ "key": "evidence_md", "value": evidence_md }],
        "tier": "mid", "temperature": "cold", "phase_flow": "single_only", "provider_fn": Value::Null,
    });
    let review: Option<String> = match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(res) => pick_review(&res).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        Err(e) => {
            let msg: String = e.chars().take(200).collect();
            return Ok(json!({ "review_md": Value::Null, "ok": false, "error": msg, "grounded": grounded }));
        }
    };

    match review {
        Some(r) => Ok(json!({ "review_md": r, "ok": true, "grounded": grounded })),
        None => Ok(json!({ "review_md": Value::Null, "ok": false, "error": "AI returned no usable review", "grounded": grounded })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT PRIMARY KEY, origin TEXT NOT NULL, type TEXT NOT NULL, path TEXT, parent_id TEXT, span_json TEXT);
             CREATE TABLE code_edges (from_node TEXT, to_node TEXT, origin TEXT NOT NULL, type TEXT NOT NULL, kind TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn empty_atlas_returns_status_with_no_files() {
        let c = setup();
        let r = atlasInsightsReview(&json!({ "origin": "atlas" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["review_md"], Value::Null);
        assert!(r["error"].as_str().unwrap().contains("empty atlas for origin 'atlas'"));
        assert_eq!(r["grounded"]["files"], json!(0));
    }

    #[test]
    fn reaches_parity_deferred_boundary_and_returns_ok_false_status_not_err() {
        let c = setup();
        c.execute("INSERT INTO code_nodes (id, origin, type, path) VALUES ('f1','atlas','file','src/core.ts')", []).unwrap();
        let out = atlasInsightsReview(&json!({ "origin": "atlas" }), &c).unwrap();
        assert_eq!(out["ok"], json!(false));
        assert_eq!(out["review_md"], Value::Null);
        assert!(out["error"].as_str().unwrap().contains("ai-call"));
        assert_eq!(out["grounded"]["files"], json!(1));
        assert_eq!(out["grounded"]["vulns"], Value::Null);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'atlas.insights.review.v2'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn top_factors_picks_top_2_positive_desc_or_dash() {
        assert_eq!(top_factors(None), "—");
        assert_eq!(top_factors(Some(&json!({ "fan_in": 0.0, "complexity": 0.0 }))), "—");
        let f = json!({ "fan_in": 0.30, "complexity": 0.90, "churn": 0.10, "uncovered": 0.0 });
        assert_eq!(top_factors(Some(&f)), "complexity 0.90, fan_in 0.30");
    }

    #[test]
    fn pick_review_finds_top_level_review_md_field() {
        let res = json!({ "output_kv": [
            { "field_name": "other", "value_text": "x", "parent_output_id": null },
            { "field_name": "review_md", "value_text": "  the brief  ", "parent_output_id": null },
        ] });
        assert_eq!(pick_review(&res), Some("  the brief  ".to_string()));
        assert_eq!(pick_review(&json!({})), None);
        // nested (non-null parent_output_id) is excluded
        let nested = json!({ "output_kv": [
            { "field_name": "review_md", "value_text": "nested", "parent_output_id": 5 },
        ] });
        assert_eq!(pick_review(&nested), None);
    }

    #[test]
    fn build_evidence_shapes_all_sections() {
        let score = json!({
            "project": { "health": 72, "risk": 28 },
            "file_count": 3,
            "churn_available": true,
            "top_risk": [{ "path": "core.ts", "risk": 80, "factors": { "complexity": 0.9, "fan_in": 0.2 } }],
        });
        let smells = json!({ "smells": [{ "severity": "high", "kind": "cycle", "detail": "circular dep" }], "counts": { "total": 1, "cycle": 1 } });
        let vulns = vec![json!({ "severity": "high", "package": "left-pad", "version": "1.0.0", "cve": "CVE-1", "fixed_in": "1.0.1" })];
        let skeleton = vec!["core.ts → utils.ts, db.ts".to_string()];
        let evidence = build_evidence(&score, &smells, Some(&vulns), &skeleton);
        assert!(evidence.contains("Architecture spine"));
        assert!(evidence.contains("Project health: 72/100 · risk: 28/100 (3 source files, churn available)."));
        assert!(evidence.contains("- 80 core.ts — complexity 0.90, fan_in 0.20"));
        assert!(evidence.contains("Structural smells (1 total — cycle:1):"));
        assert!(evidence.contains("- [high] cycle: circular dep"));
        assert!(evidence.contains("Dependency vulnerabilities (1 total — high:1):"));
        assert!(evidence.contains("- [high] left-pad@1.0.0 CVE-1 → fixed in 1.0.1"));
    }

    #[test]
    fn build_evidence_no_vulns_scanned_and_empty_sections() {
        let score = json!({ "project": { "health": 100, "risk": 0 }, "file_count": 1, "churn_available": false, "top_risk": [] });
        let smells = json!({ "smells": [], "counts": { "total": 0 } });
        let evidence = build_evidence(&score, &smells, None, &[]);
        assert!(!evidence.contains("Architecture spine"));
        assert!(evidence.contains("- (none)"));
        assert!(evidence.contains("Dependency vulnerabilities: not scanned."));
    }
}
