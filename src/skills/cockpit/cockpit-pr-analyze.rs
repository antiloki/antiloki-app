//! cockpit-pr-analyze.rs — twin of skills/cockpit/cockpit-pr-analyze.mjs (async default export →
//! sync twin per the async-lane convention: `blastRadius` is `await`ed in JS but its own Rust
//! twin (`crate::codebase_blast_radius::blastRadius`) is a plain sync `Result`-returning fn).
//! NAMES ARE CONTRACT: `cockpitPrAnalyze`. EPIC-VIBE-COCKPIT (pull requests): the "smart" layer for
//! a PR — an AI summary/documentation of the diff + a blast-radius (reverse-edge closure via the
//! atlas of what depends on the changed files). Stored back on the PR row. Both best-effort: the
//! AI branch needs a provider, the blast branch needs an onboarded atlas — either can silently
//! no-op, mirroring the JS try/catch-and-continue shape.
//!
//! PARITY-DEFERRED: the AI branch ends in `crate::ai_ai_call::aiCall` (the deferred network-AI
//! stub — see ai-call.rs's module doc), so `analysisJson`/`summary` never populate here; the twin
//! reproduces the JS's own try/catch around it (`aiCall` failing is NOT fatal to the whole
//! function — JS falls through to the blast-radius branch and still updates the row with
//! `COALESCE`d NULLs for the AI fields). `ensure_analyze_prompt` reuses the already-converted
//! `ai_prompt_find`/`ai_prompt_add` twins (mirrors pl-research.rs's `ensure_prompt`).
//!
//! Blast-radius branch: real, fully ported (deterministic, not deferred). `root` is DB-derived via
//! the already-converted `getWorkspaceRoot` twin, which per its own module doc NEVER returns
//! falsy (falls back to `SUBSTRATE_ROOT`) — so, like cockpit-pr-sync.rs, the JS `if (root)` gate is
//! unconditionally true on every real call; kept as a literal `if !root.is_empty()` check anyway
//! for structural fidelity (and as a defensive no-op should that invariant ever change). The
//! `git diff --name-only base branch` subprocess call is real (mirrors cockpit-pr-sync.rs's `git()`
//! wrapper / spawnSync-never-throws convention) — genuinely deterministic given a real git repo,
//! not a deferred boundary. `blastRadius` itself is wrapped in the same try/catch scope as JS: a
//! missing/un-onboarded atlas (`code_edges` table absent) makes `blastRadius` return `Err`, which
//! this twin swallows into `blast = None`, exactly like the JS catch.
//!
//! BESPOKE-NATIVE (playbook's VET rule) — same class as cockpit-pr-sync.rs: the blast-radius
//! branch's git subprocess call fires on every success path once a PR row is found (no input flag
//! gates it), so this unit gets real `#[cfg(test)]` tests against a temp git repo + in-memory atlas
//! tables, not a fixture/frontier entry.
// QUALITY: mixed unit — deterministic guard/row-lookup/blast-radius surface (fully ported,
// fixture-reachable) + 1 bespoke/deferred aiCall dispatch (no fixture/frontier for that branch).
use rusqlite::Connection;
use serde_json::{json, Value};

const PROMPT: &str = "cockpit.pr.analyze.v2";
const SYSTEM: &str = "You are a senior code reviewer for an autonomous build pipeline. Analyze the pull request thoroughly and return STRICT JSON with EXACTLY the required keys — no prose outside the JSON.";
const TEMPLATE: &str = "PULL REQUEST: {{title}}\n\nCONTEXT (card / contract / errors, if any):\n{{context}}\n\nDIFF:\n{{diff}}\n\nReturn ONLY a JSON object with these EXACT keys (all required, string values):\n  summary          — 1-2 sentences: what this PR does.\n  change_type      — one of: feature | fix | refactor | test | chore | docs.\n  risk_level       — one of: low | medium | high.\n  risks_md         — markdown bullets: what could break / edge cases.\n  acceptance_md    — markdown bullets: does it meet its goal/contract? acceptance checks.\n  tests_md         — markdown bullets: test coverage — present? gaps?\n  side_effects_md  — markdown bullets: side effects / blast considerations.\n  recommendation   — one of: approve | request-changes | needs-tests | comment — plus a few words why.";

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// `Number(v)` — JS's `Number(input?.id)`; NaN/0 are both falsy under `!id`.
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None | Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
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

// try promptFind; on miss, promptAdd (mirrors JS ensureAnalyzePrompt's try/catch-then-add).
fn ensure_analyze_prompt(db: &Connection) {
    if crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).is_ok() {
        return;
    }
    let _ = crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "high",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 2000,
            "output_fields": [{ "field_name": "analysis_json", "field_type": "string" }],
        }),
        db,
    );
}

/// `const git = (args, cwd) => spawnSync('git', args, { cwd, encoding: 'utf8' })` — never throws
/// (see cockpit-pr-sync.rs's module doc); a failed spawn collapses to empty stdout, matching JS's
/// `(x.stdout || '')`.
fn git_diff_names(base: &str, branch: &str, cwd: &str) -> Vec<String> {
    let out = match crate::proc::cmd("git").args(["diff", "--name-only", base, branch]).current_dir(cwd).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(_) => String::new(),
    };
    out.split('\n').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect()
}

pub fn cockpitPrAnalyze(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));
    let id_f = js_number(input.get("id"));
    if id_f == 0.0 || id_f.is_nan() {
        return Err("cockpit-pr-analyze: id is required".into());
    }
    let id = id_f as i64;
    let pr_rows = crate::db_json::query_rows(db, "SELECT * FROM cockpit_pull_requests WHERE id = ?", &[json!(id)])?;
    let pr = match pr_rows.as_array().and_then(|a| a.first()) {
        Some(p) => p.clone(),
        None => return Ok(json!({ "ok": false, "error": format!("PR {id} not found") })),
    };

    // High-tier TEMPLATED analysis from the diff + context — PARITY-DEFERRED (see module doc):
    // aiCall always errors, so analysis_json/summary stay None, mirroring JS's catch-and-skip.
    let analysis_json: Option<String> = None;
    let summary: Option<String> = None;
    {
        ensure_analyze_prompt(db);
        let title = str_qq(pr.get("title"));
        // `String(input?.context || '(none)')` — `||` falls back on any falsy context value.
        let context_v = input.get("context").cloned().unwrap_or(Value::Null);
        let ctx_raw = if truthy(Some(&context_v)) { str_qq(Some(&context_v)) } else { "(none)".to_string() };
        let ctx: String = ctx_raw.chars().take(8000).collect();
        let patch = str_qq(pr.get("patch"));
        let patch = if patch.is_empty() { "(empty)".to_string() } else { patch };
        let diff: String = patch.chars().take(40000).collect();
        let ai_input = json!({
            "prompt_name": PROMPT,
            "tier": "high",
            "input_kv": [
                { "key": "title", "value": title },
                { "key": "context", "value": ctx },
                { "key": "diff", "value": diff },
            ],
        });
        let _ = crate::ai_ai_call::aiCall(&ai_input, db); // Err (deferred) → swallowed, same as JS catch
    }

    // blast-radius from the changed files (reverse-dependency closure via the atlas)
    let mut blast: Option<Value> = None;
    {
        let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
        let root = root_v.as_str().unwrap_or_default().to_string();
        if !root.is_empty() {
            let base_branch = str_qq(pr.get("base_branch"));
            let branch = str_qq(pr.get("branch"));
            let names = git_diff_names(&base_branch, &branch, &root);
            if !names.is_empty() {
                let node_ids: Vec<Value> = names.iter().map(|n| json!(format!("{n}@atlas"))).collect();
                if let Ok(r) = crate::codebase_blast_radius::blastRadius(&json!({ "node_ids": node_ids }), db) {
                    let affected = r.get("affected").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
                    let affected_tests = r.get("affected_tests").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
                    let files: Vec<Value> = names.iter().take(50).map(|n| json!(n)).collect();
                    blast = Some(json!({ "changed": names.len(), "affected": affected, "affected_tests": affected_tests, "files": files }));
                }
                // else: atlas not onboarded (blastRadius Err) → blast stays None, same as JS catch.
            }
        }
    }

    crate::db_json::run_write(
        db,
        "UPDATE cockpit_pull_requests SET analysis_json = COALESCE(?, analysis_json), summary_md = COALESCE(?, summary_md), blast_json = COALESCE(?, blast_json), updated_at = datetime('2026-01-01 00:00:00') WHERE id = ?",
        &[
            analysis_json.map(|s| json!(s)).unwrap_or(Value::Null),
            summary.map(|s| json!(s)).unwrap_or(Value::Null),
            blast.as_ref().map(|b| json!(serde_json::to_string(b).unwrap_or_default())).unwrap_or(Value::Null),
            json!(id),
        ],
    )?;

    Ok(json!({ "ok": true, "id": id, "analysis": Value::Null, "blast": blast }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn run_git(args: &[&str], cwd: &Path) {
        let o = crate::proc::cmd("git").args(args).current_dir(cwd).output().unwrap();
        assert!(o.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&o.stderr));
    }

    fn make_temp_git_repo(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-cockpit-pr-analyze-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        run_git(&["init", "-q"], &dir);
        run_git(&["symbolic-ref", "HEAD", "refs/heads/main"], &dir);
        run_git(&["config", "user.email", "test@test"], &dir);
        run_git(&["config", "user.name", "test"], &dir);
        fs::write(dir.join("file.txt"), "line1\n").unwrap();
        run_git(&["add", "file.txt"], &dir);
        run_git(&["commit", "-q", "-m", "init"], &dir);
        dir
    }

    fn db_with_workspace(root: &Path) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00'); \
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) ); \
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        )
        .unwrap();
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', ?1, 1)", rusqlite::params![root.to_string_lossy().into_owned()]).unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c
    }

    #[test]
    fn id_is_required() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        let e = cockpitPrAnalyze(&json!({}), &c).unwrap_err();
        assert_eq!(e, "cockpit-pr-analyze: id is required");
        let e2 = cockpitPrAnalyze(&json!({ "id": 0 }), &c).unwrap_err();
        assert_eq!(e2, "cockpit-pr-analyze: id is required");
    }

    #[test]
    fn pr_not_found_returns_ok_false() {
        let dir = make_temp_git_repo("notfound");
        let c = db_with_workspace(&dir);
        let r = cockpitPrAnalyze(&json!({ "id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "PR 999 not found" }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn analyzes_pr_computes_blast_radius_when_atlas_onboarded() {
        let dir = make_temp_git_repo("blast");
        run_git(&["checkout", "-q", "-b", "antiloki/feature"], &dir);
        fs::write(dir.join("src.rs"), "changed\n").unwrap();
        run_git(&["add", "src.rs"], &dir);
        run_git(&["commit", "-q", "-m", "feature work"], &dir);
        run_git(&["checkout", "-q", "main"], &dir);

        let c = db_with_workspace(&dir);
        c.execute_batch(
            "CREATE TABLE code_edges (from_node TEXT, to_node TEXT, kind TEXT); \
             CREATE TABLE code_nodes (id TEXT, type TEXT, path TEXT); \
             INSERT INTO code_edges (from_node, to_node) VALUES ('caller.rs@atlas', 'src.rs@atlas');",
        )
        .unwrap();
        c.execute(
            "INSERT INTO cockpit_pull_requests (session_id, source, branch, base_branch, title, status) VALUES (0, 'vibe', 'antiloki/feature', 'main', 'my PR', 'open')",
            [],
        )
        .unwrap();
        let id: i64 = c.query_row("SELECT id FROM cockpit_pull_requests WHERE branch = 'antiloki/feature'", [], |r| r.get(0)).unwrap();

        let r = cockpitPrAnalyze(&json!({ "id": id }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["id"], json!(id));
        assert_eq!(r["analysis"], Value::Null); // aiCall deferred → analysis stays null

        let blast = &r["blast"];
        assert_eq!(blast["changed"], json!(1));
        assert_eq!(blast["affected"], json!(1)); // caller.rs@atlas depends on the changed src.rs
        assert_eq!(blast["files"], json!(["src.rs"]));

        let row = crate::db_json::query_rows(&c, "SELECT blast_json, analysis_json, summary_md FROM cockpit_pull_requests WHERE id = ?", &[json!(id)])
            .unwrap();
        let row = row.as_array().unwrap().first().unwrap();
        assert!(row["blast_json"].as_str().unwrap().contains("\"changed\":1"));
        assert_eq!(row["analysis_json"], Value::Null);
        assert_eq!(row["summary_md"], Value::Null);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn blast_stays_null_when_atlas_not_onboarded_no_code_edges_table() {
        let dir = make_temp_git_repo("noatlas");
        run_git(&["checkout", "-q", "-b", "antiloki/lonely"], &dir);
        fs::write(dir.join("x.rs"), "x\n").unwrap();
        run_git(&["add", "x.rs"], &dir);
        run_git(&["commit", "-q", "-m", "x"], &dir);
        run_git(&["checkout", "-q", "main"], &dir);

        let c = db_with_workspace(&dir);
        c.execute(
            "INSERT INTO cockpit_pull_requests (session_id, source, branch, base_branch, title, status) VALUES (0, 'vibe', 'antiloki/lonely', 'main', 'lonely PR', 'open')",
            [],
        )
        .unwrap();
        let id: i64 = c.query_row("SELECT id FROM cockpit_pull_requests WHERE branch = 'antiloki/lonely'", [], |r| r.get(0)).unwrap();

        let r = cockpitPrAnalyze(&json!({ "id": id }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["blast"], Value::Null); // code_edges table absent -> blastRadius Err -> swallowed
    }

    #[test]
    fn no_changed_files_leaves_blast_null() {
        let dir = make_temp_git_repo("nochange");
        let c = db_with_workspace(&dir);
        c.execute_batch("CREATE TABLE code_edges (from_node TEXT, to_node TEXT, kind TEXT);").unwrap();
        c.execute(
            "INSERT INTO cockpit_pull_requests (session_id, source, branch, base_branch, title, status) VALUES (0, 'vibe', 'main', 'main', 'noop PR', 'open')",
            [],
        )
        .unwrap();
        let id: i64 = c.query_row("SELECT id FROM cockpit_pull_requests WHERE branch = 'main'", [], |r| r.get(0)).unwrap();

        let r = cockpitPrAnalyze(&json!({ "id": id }), &c).unwrap();
        assert_eq!(r["blast"], Value::Null); // base==branch -> no diff names -> blast branch never runs
    }
}
