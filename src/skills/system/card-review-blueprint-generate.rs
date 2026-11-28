//! card-review-blueprint-generate.rs — twin of skills/system/card-review-blueprint-generate.mjs.
//! NAMES ARE CONTRACT: `cardReviewBlueprintGenerate` (default) · `assembleNarrative` · `hydrate`.
//!
//! BESPOKE-NATIVE (playbook lane B): this skill composes `reviewDiffCompute`
//! (crate::system_review_diff_compute::reviewDiffCompute), which is itself bespoke-native — it
//! shells out to a real `git` subprocess against a real repo on disk. There is no way to make
//! this skill's own conform-db fixture deterministic without faking that dependency, so per the
//! crossing playbook's "dispatcher over a now-twinned bespoke-native skill" rule this twin is
//! bespoke-native too, proven by native `#[cfg(test)]` tests against a real temp-dir git repo
//! (mirrors review-diff-compute.rs / card-commits.rs).
//!
//! Writes its own row directly to `card_review_blueprints` (not via with-audit) — same as the JS
//! source's own doc comment: "It does NOT mutate audited entities, so it writes its own row
//! directly (same status as man_reports, per migration 109)."
//!
//! `exec_fn` (JS's injectable git runner for the JS unit tests) has no twin equivalent — this
//! twin always calls the real `reviewDiffCompute`, which always shells out to real `git`.
// QUALITY: bespoke-native (composes a real-subprocess twin on every call) — no fixture/frontier
// entry; correctness proven by the native tests below against a real temp-dir git repo.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

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
}

fn resolve_contract_slug(input: &Value, db: &Connection) -> Result<Option<String>, String> {
    if let Some(s) = input.get("contract_slug").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        return Ok(Some(s.to_string()));
    }
    let card_slug = match input.get("card_slug").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => return Ok(None),
    };
    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug FROM contracts WHERE card_slug = ? ORDER BY current_attempt DESC, id DESC LIMIT 1",
        &[json!(card_slug)],
    )?;
    Ok(rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("slug")).and_then(Value::as_str).map(String::from))
}

fn parse_microtasks(contract: &Value) -> Vec<Value> {
    contract
        .get("microtasks_json")
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

fn card_why(db: &Connection, card_slug: &str) -> Result<Value, String> {
    let rows = crate::db_json::query_rows(db, "SELECT title, body_json FROM cards WHERE slug = ?", &[json!(card_slug)])?;
    let row = rows.as_array().and_then(|a| a.first());
    let title = row.and_then(|r| r.get("title")).and_then(Value::as_str).unwrap_or(card_slug).to_string();
    let why = row
        .and_then(|r| r.get("body_json"))
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|b| b.get("why").cloned())
        .filter(|v| !v.is_null())
        .map(|v| str_qq(Some(&v)))
        .unwrap_or_default();
    Ok(json!({ "title": title, "why": why }))
}

/// `assembleNarrative({ card, contract, microtasks, manReport, verdict, anchors, behavioral })`.
pub fn assembleNarrative(args: &Value) -> String {
    let card = args.get("card").cloned().unwrap_or(json!({}));
    let contract = args.get("contract").cloned().unwrap_or(json!({}));
    let microtasks = args.get("microtasks").and_then(Value::as_array).cloned().unwrap_or_default();
    let man_report = args.get("manReport").filter(|v| !v.is_null());
    let verdict = args.get("verdict").filter(|v| !v.is_null());
    let anchors = args.get("anchors").and_then(Value::as_array).cloned().unwrap_or_default();
    let behavioral = args.get("behavioral").and_then(Value::as_array).cloned().unwrap_or_default();

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("# Review blueprint — {}", str_qq(card.get("title"))));
    lines.push(String::new());
    lines.push(format!("**Verdict:** {}", verdict.map(|v| str_qq(Some(v))).unwrap_or_else(|| "unknown".to_string())));
    lines.push(String::new());
    let why = card.get("why").filter(|v| truthy(Some(v)));
    if let Some(w) = why {
        lines.push("## Why".to_string());
        lines.push(str_qq(Some(w)));
        lines.push(String::new());
    }
    if let Some(g) = contract.get("goal_md").filter(|v| truthy(Some(v))) {
        lines.push("## Goal".to_string());
        lines.push(str_qq(Some(g)).trim().to_string());
        lines.push(String::new());
    }
    if let Some(t) = contract.get("tl_briefing_md").filter(|v| truthy(Some(v))) {
        lines.push("## TL briefing".to_string());
        lines.push(str_qq(Some(t)).trim().to_string());
        lines.push(String::new());
    }
    if !microtasks.is_empty() {
        lines.push("## Microtasks".to_string());
        for m in &microtasks {
            let kind = m.get("kind").filter(|v| truthy(Some(v))).map(|v| str_qq(Some(v))).unwrap_or_else(|| "task".to_string());
            let intent = m
                .get("intent")
                .filter(|v| truthy(Some(v)))
                .or_else(|| m.get("microtask_id"))
                .filter(|v| truthy(Some(v)))
                .map(|v| str_qq(Some(v)))
                .unwrap_or_default();
            lines.push(format!("- [{kind}] {intent}").trim_end().to_string());
        }
        lines.push(String::new());
    }
    let total_ac = anchors.len() + behavioral.len();
    let failed = anchors
        .iter()
        .chain(behavioral.iter())
        .filter(|a| a.get("passed") == Some(&Value::Bool(false)))
        .count();
    lines.push("## Acceptance".to_string());
    lines.push(format!("{total_ac} criteria · {} anchored inline · {} behavioral · {failed} failing", anchors.len(), behavioral.len()));
    lines.push(String::new());
    if let Some(mr) = man_report {
        if let Some(lessons) = mr.get("lessons_md").filter(|v| truthy(Some(v))) {
            lines.push("## Lessons (MAN review)".to_string());
            lines.push(str_qq(Some(lessons)).trim().to_string());
            lines.push(String::new());
        }
    }
    lines.join("\n")
}

/// JS: `function hydrate(row) { if (!row) return null; ... }` — parses the JSON columns onto the
/// row for caller convenience, keeping the raw `_json` columns too.
pub fn hydrate(row: Option<Value>) -> Value {
    let mut out = match row {
        Some(Value::Object(m)) => m,
        _ => return Value::Null,
    };
    for col in ["diff_json", "anchors_json", "behavioral_json"] {
        let key = &col[..col.len() - "_json".len()];
        let parsed = out
            .get(col)
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .unwrap_or(Value::Null);
        out.insert(key.to_string(), parsed);
    }
    Value::Object(out)
}

fn latest_blueprint(db: &Connection, card_slug: &str, contract_slug: &str, attempt: i64) -> Result<Option<Value>, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT * FROM card_review_blueprints WHERE card_slug = ? AND contract_slug = ? AND attempt = ?",
        &[json!(card_slug), json!(contract_slug), json!(attempt)],
    )?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned())
}

/// `export default async function cardReviewBlueprintGenerate(input, context)` — async ignored
/// (sync SQLite reads + a sync subprocess call via reviewDiffCompute resolve to plain sync Rust).
pub fn cardReviewBlueprintGenerate(input: &Value, db: &Connection) -> Result<Value, String> {
    let contract_slug = resolve_contract_slug(input, db)?;
    let contract_slug = match contract_slug {
        Some(s) => s,
        None => {
            return Err(
                "card-review-blueprint-generate: no contract resolvable from input (need contract_slug or a card_slug with a contract)"
                    .to_string(),
            )
        }
    };

    let contract = crate::contract_contract_get::contractGet(&json!(contract_slug), db)?;
    if contract.is_null() {
        return Err(format!("card-review-blueprint-generate: contract '{contract_slug}' not found"));
    }

    let card_slug = contract.get("card_slug").and_then(Value::as_str).unwrap_or("").to_string();
    let attempt = input
        .get("attempt")
        .filter(|v| !v.is_null())
        .and_then(Value::as_i64)
        .or_else(|| contract.get("current_attempt").and_then(Value::as_i64))
        .unwrap_or(1);
    let force = truthy(input.get("force"));
    let generated_by = input
        .get("generated_by")
        .filter(|v| truthy(Some(v)))
        .map(|v| str_qq(Some(v)))
        .unwrap_or_else(|| "unknown".to_string());

    let existing = latest_blueprint(db, &card_slug, &contract_slug, attempt)?;
    if let Some(row) = existing.clone() {
        if !force {
            return Ok(hydrate(Some(row)));
        }
    }

    let qa_run_rows = crate::db_json::query_rows(
        db,
        "SELECT * FROM qa_runs WHERE contract_slug = ? ORDER BY ts DESC, id DESC LIMIT 1",
        &[json!(contract_slug)],
    )?;
    let qa_run = qa_run_rows.as_array().and_then(|a| a.first()).cloned();
    let qa_results: Vec<Value> = match &qa_run {
        Some(r) => crate::db_json::query_rows(
            db,
            "SELECT ac_index, kind, passed, evidence_md FROM qa_run_results WHERE qa_run_id = ? ORDER BY ac_index",
            &[r.get("id").cloned().unwrap_or(Value::Null)],
        )?
        .as_array()
        .cloned()
        .unwrap_or_default(),
        None => vec![],
    };

    let man_report_rows = crate::db_json::query_rows(
        db,
        "SELECT verdict, lessons_md, what_happened_md FROM man_reports WHERE contract_slug = ? ORDER BY ts DESC, id DESC LIMIT 1",
        &[json!(contract_slug)],
    )?;
    let man_report = man_report_rows.as_array().and_then(|a| a.first()).cloned();

    let diff = crate::system_review_diff_compute::reviewDiffCompute(
        &json!({ "contract_slug": contract_slug, "head_sha": input.get("head_sha").cloned().unwrap_or(Value::Null) }),
        db,
    )?;

    let acceptance = contract.get("acceptance").and_then(Value::as_array).cloned().unwrap_or_default();
    let anchor_input = json!({ "acceptance": acceptance, "qa_results": qa_results, "diff": diff });
    let anchor_out = crate::system_review_anchor_resolve::reviewAnchorResolve(&anchor_input, db)?;
    let anchors = anchor_out.get("anchors").and_then(Value::as_array).cloned().unwrap_or_default();
    let behavioral = anchor_out.get("behavioral").and_then(Value::as_array).cloned().unwrap_or_default();

    let card = card_why(db, &card_slug)?;
    let microtasks = parse_microtasks(&contract);
    let verdict = man_report
        .as_ref()
        .and_then(|m| m.get("verdict").cloned())
        .filter(|v| !v.is_null())
        .or_else(|| {
            qa_run.as_ref().map(|r| {
                if truthy(r.get("all_passed")) { json!("qa-passed") } else { json!("qa-failed") }
            })
        })
        .unwrap_or(Value::Null);

    let narrative_md = assembleNarrative(&json!({
        "card": card, "contract": contract, "microtasks": microtasks, "manReport": man_report,
        "verdict": verdict, "anchors": anchors, "behavioral": behavioral,
    }));

    let diff_json = serde_json::to_string(diff.get("files").cloned().unwrap_or(json!([])).as_array().unwrap()).unwrap();
    let anchors_json = serde_json::to_string(&anchors).unwrap();
    let behavioral_json = serde_json::to_string(&behavioral).unwrap();
    let base_sha = diff.get("base_sha").cloned().unwrap_or(Value::Null);
    let head_sha = diff.get("head_sha").cloned().unwrap_or(Value::Null);
    let diff_source = diff.get("diff_source").cloned().unwrap_or(Value::Null);
    let qa_run_id = qa_run.as_ref().and_then(|r| r.get("id").cloned()).unwrap_or(Value::Null);

    if existing.is_some() && force {
        crate::db_json::run_write(
            db,
            "UPDATE card_review_blueprints
             SET qa_run_id=?, base_sha=?, head_sha=?, diff_source=?,
                 diff_json=?, anchors_json=?, behavioral_json=?,
                 narrative_md=?, verdict=?, generated_by=?, generated_at=datetime('now')
             WHERE card_slug=? AND contract_slug=? AND attempt=?",
            &[
                qa_run_id, base_sha, head_sha, diff_source,
                json!(diff_json), json!(anchors_json), json!(behavioral_json),
                json!(narrative_md), verdict, json!(generated_by),
                json!(card_slug), json!(contract_slug), json!(attempt),
            ],
        )?;
    } else {
        crate::db_json::run_write(
            db,
            "INSERT INTO card_review_blueprints
                (card_slug, contract_slug, attempt, qa_run_id, base_sha, head_sha, diff_source,
                 diff_json, anchors_json, behavioral_json, narrative_md, verdict, generated_by)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            &[
                json!(card_slug), json!(contract_slug), json!(attempt), qa_run_id, base_sha, head_sha, diff_source,
                json!(diff_json), json!(anchors_json), json!(behavioral_json), json!(narrative_md), verdict, json!(generated_by),
            ],
        )?;
    }

    let saved = latest_blueprint(db, &card_slug, &contract_slug, attempt)?;
    Ok(hydrate(saved))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::fs;

    // SAFETY: ANTILOKI_SUBSTRATE_ROOT is process-global; shared crate-wide lock (never a local
    // mutex — same convention as review-diff-compute.rs / card-commits.rs).
    fn with_substrate_root<T>(root: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", root); }
        let result = f();
        unsafe { std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT"); }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-crbg-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn git(args: &[&str], cwd: &Path) {
        let status = Command::new("git").args(args).current_dir(cwd).status().unwrap();
        assert!(status.success(), "git {args:?} failed in {cwd:?}");
    }
    fn init_repo(dir: &Path) {
        git(&["init", "-q"], dir);
        git(&["config", "user.email", "test@example.com"], dir);
        git(&["config", "user.name", "Test"], dir);
    }
    fn commit_file(dir: &Path, file: &str, contents: &str, message: &str) {
        if let Some(parent) = Path::new(file).parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(dir.join(parent)).unwrap();
            }
        }
        fs::write(dir.join(file), contents).unwrap();
        git(&["add", "-A"], dir);
        git(&["commit", "-m", message], dir);
    }
    fn head_sha(dir: &Path) -> String {
        let out = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, card_slug TEXT,
                current_attempt INTEGER, base_sha TEXT, sandbox_path TEXT, files_touched_json TEXT,
                acceptance_json TEXT, naming_json TEXT, edge_cases_json TEXT, must_obey_json TEXT,
                out_of_scope_json TEXT, reference_artifacts_json TEXT, generated_paths_json TEXT,
                goal_md TEXT, tl_briefing_md TEXT, microtasks_json TEXT);
             CREATE TABLE cards (slug TEXT, title TEXT, body_json TEXT);
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, contract_slug TEXT, all_passed INTEGER);
             CREATE TABLE qa_run_results (id INTEGER PRIMARY KEY AUTOINCREMENT, qa_run_id INTEGER, ac_index INTEGER, kind TEXT, passed INTEGER, evidence_md TEXT);
             CREATE TABLE man_reports (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, ts TEXT, verdict TEXT, lessons_md TEXT, what_happened_md TEXT);
             CREATE TABLE card_review_blueprints (
                id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, contract_slug TEXT, attempt INTEGER,
                qa_run_id INTEGER, base_sha TEXT, head_sha TEXT, diff_source TEXT,
                diff_json TEXT, anchors_json TEXT, behavioral_json TEXT, narrative_md TEXT,
                verdict TEXT, generated_by TEXT, generated_at TEXT DEFAULT (datetime('2026-01-01 00:00:00')));",
        )
        .unwrap();
        c
    }

    fn seed_contract(c: &Connection, slug: &str, card_slug: &str, base_sha: &str) {
        c.execute(
            "INSERT INTO contracts (slug, card_slug, current_attempt, base_sha, files_touched_json, acceptance_json, goal_md, microtasks_json)
             VALUES (?1, ?2, 1, ?3, '[\"src/foo.txt\"]', '[{\"kind\":\"file_exists\",\"path\":\"src/foo.txt\",\"must_contain\":[\"changed\"]}]', 'Ship it', '[{\"kind\":\"code\",\"intent\":\"do it\"}]')",
            [slug, card_slug, base_sha],
        ).unwrap();
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES (?1, ?1, '{\"why\":\"because\"}')", [card_slug]).unwrap();
    }

    #[test]
    fn missing_contract_returns_exact_error() {
        let c = db();
        let e = cardReviewBlueprintGenerate(&json!({}), &c).unwrap_err();
        assert_eq!(e, "card-review-blueprint-generate: no contract resolvable from input (need contract_slug or a card_slug with a contract)");
        let e2 = cardReviewBlueprintGenerate(&json!({ "contract_slug": "GHOST" }), &c).unwrap_err();
        assert_eq!(e2, "card-review-blueprint-generate: contract 'GHOST' not found");
    }

    #[test]
    fn generates_and_persists_a_blueprint_then_is_idempotent() {
        let dir = temp_dir("gen");
        init_repo(&dir);
        commit_file(&dir, "src/foo.txt", "line1\n", "base commit");
        let base = head_sha(&dir);
        commit_file(&dir, "src/foo.txt", "line1\nchanged\n", "head commit");
        let head = head_sha(&dir);

        let c = db();
        seed_contract(&c, "CT-1", "CARD-1", &base);
        let rid = { c.execute("INSERT INTO qa_runs (ts, contract_slug, all_passed) VALUES ('2026-01-01', 'CT-1', 1)", []).unwrap(); c.last_insert_rowid() };
        c.execute("INSERT INTO qa_run_results (qa_run_id, ac_index, kind, passed, evidence_md) VALUES (?1, 0, 'file_exists', 1, 'ok')", [rid]).unwrap();

        let root = dir.to_string_lossy().into_owned();
        let r = with_substrate_root(&root, || {
            cardReviewBlueprintGenerate(&json!({ "contract_slug": "CT-1", "head_sha": head }), &c).unwrap()
        });
        assert_eq!(r["card_slug"], json!("CARD-1"));
        assert_eq!(r["contract_slug"], json!("CT-1"));
        assert_eq!(r["attempt"], json!(1));
        assert_eq!(r["verdict"], json!("qa-passed"));
        assert_eq!(r["diff_source"], json!("git"));
        assert!(r["narrative_md"].as_str().unwrap().contains("Review blueprint — CARD-1"));
        assert!(r["narrative_md"].as_str().unwrap().contains("**Verdict:** qa-passed"));
        assert_eq!(r["anchors"].as_array().unwrap().len(), 1);
        assert_eq!(r["behavioral"], json!([]));

        let count: i64 = c.query_row("SELECT COUNT(*) FROM card_review_blueprints", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);

        // Idempotent without force: second call returns the SAME row, does not insert again.
        let r2 = with_substrate_root(&root, || {
            cardReviewBlueprintGenerate(&json!({ "contract_slug": "CT-1" }), &c).unwrap()
        });
        assert_eq!(r2["id"], r["id"]);
        let count2: i64 = c.query_row("SELECT COUNT(*) FROM card_review_blueprints", [], |row| row.get(0)).unwrap();
        assert_eq!(count2, 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn force_re_snapshots_the_existing_row() {
        let dir = temp_dir("force");
        init_repo(&dir);
        commit_file(&dir, "src/foo.txt", "line1\n", "base commit");
        let base = head_sha(&dir);
        commit_file(&dir, "src/foo.txt", "line1\nchanged\n", "head commit");
        let head = head_sha(&dir);

        let c = db();
        seed_contract(&c, "CT-2", "CARD-2", &base);
        let root = dir.to_string_lossy().into_owned();
        let first = with_substrate_root(&root, || {
            cardReviewBlueprintGenerate(&json!({ "contract_slug": "CT-2", "head_sha": head, "generated_by": "alice" }), &c).unwrap()
        });
        let forced = with_substrate_root(&root, || {
            cardReviewBlueprintGenerate(&json!({ "contract_slug": "CT-2", "head_sha": head, "force": true, "generated_by": "bob" }), &c).unwrap()
        });
        assert_eq!(forced["id"], first["id"]);
        assert_eq!(forced["generated_by"], json!("bob"));
        let count: i64 = c.query_row("SELECT COUNT(*) FROM card_review_blueprints", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn card_slug_alone_resolves_latest_contract_by_attempt() {
        let dir = temp_dir("cardslug");
        init_repo(&dir);
        commit_file(&dir, "src/foo.txt", "line1\n", "base commit");
        let base = head_sha(&dir);

        let c = db();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, current_attempt, base_sha, files_touched_json, acceptance_json)
             VALUES ('CT-OLD', 'CARD-3', 1, ?1, '[]', '[]')", [&base],
        ).unwrap();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, current_attempt, base_sha, files_touched_json, acceptance_json)
             VALUES ('CT-NEW', 'CARD-3', 2, ?1, '[]', '[]')", [&base],
        ).unwrap();
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('CARD-3', 'Card Three', NULL)", []).unwrap();

        let root = dir.to_string_lossy().into_owned();
        let r = with_substrate_root(&root, || {
            cardReviewBlueprintGenerate(&json!({ "card_slug": "CARD-3" }), &c).unwrap()
        });
        assert_eq!(r["contract_slug"], json!("CT-NEW"));
        assert_eq!(r["attempt"], json!(2));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn assemble_narrative_matches_shape() {
        let s = assembleNarrative(&json!({
            "card": { "title": "T", "why": "" },
            "contract": {},
            "microtasks": [],
            "manReport": Value::Null,
            "verdict": Value::Null,
            "anchors": [],
            "behavioral": [],
        }));
        assert!(s.starts_with("# Review blueprint — T\n\n**Verdict:** unknown\n\n## Acceptance\n0 criteria"));
    }

    #[test]
    fn hydrate_parses_json_columns_and_null_passthrough() {
        assert_eq!(hydrate(None), Value::Null);
        let row = json!({ "diff_json": "[{\"a\":1}]", "anchors_json": "bad", "behavioral_json": null, "verdict": "qa-passed" });
        let h = hydrate(Some(row));
        assert_eq!(h["diff"], json!([{ "a": 1 }]));
        assert_eq!(h["anchors"], Value::Null);
        assert_eq!(h["behavioral"], Value::Null);
        assert_eq!(h["verdict"], json!("qa-passed"));
    }
}
