//! onboarding-orchestrate.rs — twin of skills/onboarding/onboarding-orchestrate.mjs. NAMES ARE
//! CONTRACT: `onboardingOrchestrate` (default, the only export).
//!
//! BESPOKE-NATIVE (onboarding-scan.rs precedent): this fn's step 1 unconditionally calls
//! `onboarding_onboarding_scan::onboardingScan`, which itself unconditionally shells out to
//! `git log` via `scanGitHistory` (no input flag selects a git-free path — see onboarding-scan.rs's
//! module doc). Since every invocation of this orchestrator transitively runs a real subprocess +
//! real filesystem walk, it inherits the same "not conditionally avoidable" non-determinism and is
//! NOT conform-db fixtured — no `fixtures/onboarding_onboarding_orchestrate__onboarding_orchestrate.json`,
//! no frontier entry. Proven correct instead by `#[cfg(test)]` native tests against a real temp-dir
//! git repo + a real in-memory rusqlite db with `memory`/`knowledge` attached (onboarding-write.rs's
//! test harness) plus a `cards` table (this fn's own step 4 write). Guard-throws (missing `repo_path`)
//! are ported for real but — per the "guard-throws are not exercised" convention every sibling twin
//! uses — not covered by a dedicated fixture case beyond the native tests below.
//!
//! Steps 2 (`onboardingInfer`) and 3 (`onboardingWrite`) ARE conform-db fixtured on their own twins;
//! calling them here from already-real code is cross-module composition (playbook gotcha 11), not a
//! new source of non-determinism.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand: nz(a).or(nz(b))

const FROZEN_ISO: &str = "2026-01-01T00:00:00.000Z"; // new Date().toISOString() — frozen-clock lane

fn last_sep(p: &str) -> Option<usize> { p.rfind(|c| c == '/' || c == '\\') }
// node:path basename(p) (no ext arg) — win32 impl treats BOTH separators as splits.
fn basename(p: &str) -> String {
    let trimmed = p.trim_end_matches(|c| c == '/' || c == '\\');
    match last_sep(trimmed) { Some(i) => trimmed[i + 1..].to_string(), None => trimmed.to_string() }
}

// `{ name, at: ..., ...payload }` — payload entries whose value is `None` are OMITTED (JS
// `JSON.stringify` drops an explicit `undefined` value, gotcha 3), matching `?.` chains with no
// `?? default` in the JS source (e.g. `inference.architecture?.layer_count`).
fn record(phases: &mut Vec<Value>, name: &str, payload: Vec<(&str, Option<Value>)>) {
    let mut m = Map::new();
    m.insert("name".to_string(), json!(name));
    m.insert("at".to_string(), json!(FROZEN_ISO));
    for (k, v) in payload {
        if let Some(v) = v { m.insert(k.to_string(), v); }
    }
    phases.push(Value::Object(m));
}

fn arr_len(v: Option<&Value>) -> Option<i64> {
    v.and_then(Value::as_array).map(|a| a.len() as i64)
}

pub fn onboardingOrchestrate(input: &Value, db: &Connection) -> Result<Value, String> {
    let repo_path_v = nz(input.get("repo_path"));
    if !truthy(repo_path_v) {
        return Err("onboarding-orchestrate: repo_path is required".to_string());
    }
    let repo_path = repo_path_v.and_then(Value::as_str).unwrap_or_default();

    // `dry_run = false` / `propose_starter_card = true` — JS default-parameter destructuring
    // applies only when the key is MISSING or explicitly `undefined`; an explicit `null` is kept.
    let dry_run_v: Value = match input.get("dry_run") { None => json!(false), Some(v) => v.clone() };
    let dry_run = truthy(Some(&dry_run_v));
    let propose_v: Value = match input.get("propose_starter_card") { None => json!(true), Some(v) => v.clone() };
    let propose_starter_card = truthy(Some(&propose_v));

    let mut phases: Vec<Value> = Vec::new();

    // 1. Scan
    let scan = crate::onboarding_onboarding_scan::onboardingScan(&json!({ "repo_path": repo_path }), db)?;
    record(&mut phases, "scan", vec![
        ("file_count", scan.get("file_count").cloned()),
        ("languages", Some(json!(arr_len(scan.get("stack").and_then(|s| s.get("languages"))).unwrap_or(0)))),
    ]);

    // 2. Infer
    let inference = crate::onboarding_onboarding_infer::onboardingInfer(&json!({ "scan": scan }), db)?;
    record(&mut phases, "infer", vec![
        ("layers", inference.get("architecture").and_then(|a| a.get("layer_count")).cloned()),
        ("patterns", arr_len(inference.get("patterns").and_then(|p| p.get("patterns"))).map(|n| json!(n))),
        ("entities", inference.get("domain").and_then(|d| d.get("entity_count")).cloned()),
    ]);

    // 3. Write (unless dry-run)
    let mut write_result: Value = json!({ "memories_written": 0, "knowledge_written": 0, "skipped": dry_run_v.clone() });
    if !dry_run {
        write_result = crate::onboarding_onboarding_write::onboardingWrite(&json!({ "scan": scan, "inference": inference }), db)?;
        record(&mut phases, "write", vec![
            ("source", write_result.get("source").cloned()),
            ("memories_written", write_result.get("memories_written").cloned()),
            ("knowledge_written", write_result.get("knowledge_written").cloned()),
        ]);
    } else {
        record(&mut phases, "write", vec![("skipped", Some(json!(true)))]);
    }

    // 4. Propose starter card
    let mut proposed_card: Value = Value::Null;
    if propose_starter_card && !dry_run {
        let base = basename(repo_path).to_uppercase();
        let cleaned: String = base.chars().map(|c| if c == '-' || c.is_ascii_digit() || c.is_ascii_uppercase() { c } else { '-' }).collect();
        let slug = format!("EPIC-ONBOARDED-{}", cleaned.chars().take(24).collect::<String>());
        let title = format!("Onboarded {} — initial scan + memories seeded", basename(repo_path));
        let insert = crate::db_json::run_write(
            db,
            "INSERT OR IGNORE INTO cards (slug, title, status) VALUES (?, ?, 'backlog')",
            &[json!(slug), json!(title)],
        );
        if insert.is_ok() {
            proposed_card = json!({ "slug": slug, "title": title, "status": "backlog" });
        }
        record(&mut phases, "propose", vec![("card", Some(proposed_card.clone()))]);
    }

    let report = json!({
        "repo_path": scan.get("repo_path").cloned().unwrap_or(Value::Null),
        "file_count": scan.get("file_count").cloned().unwrap_or(Value::Null),
        "by_kind": scan.get("by_kind").cloned().unwrap_or(Value::Null),
        "stack": scan.get("stack").cloned().unwrap_or(Value::Null),
        "git_history_available": nz(scan.get("git_history").and_then(|g| g.get("available"))).cloned().unwrap_or(json!(false)),
        "docs_found": arr_len(scan.get("docs")).unwrap_or(0),
        "architecture": inference.get("architecture").cloned().unwrap_or(Value::Null),
        "pattern_count": arr_len(inference.get("patterns").and_then(|p| p.get("patterns"))).unwrap_or(0),
        "anti_pattern_count": arr_len(inference.get("patterns").and_then(|p| p.get("anti_patterns"))).unwrap_or(0),
        "domain_entity_count": nz(inference.get("domain").and_then(|d| d.get("entity_count"))).cloned().unwrap_or(json!(0)),
        "memories_written": write_result.get("memories_written").cloned().unwrap_or(json!(0)),
        "knowledge_written": write_result.get("knowledge_written").cloned().unwrap_or(json!(0)),
        "proposed_card": proposed_card,
        "phases": phases,
        "dry_run": dry_run_v,
    });
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-onboarding-orchestrate-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn git_ok(args: &[&str], cwd: &Path) {
        let out = crate::proc::cmd("git").args(args).current_dir(cwd).output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }
    fn fixture_repo(tag: &str) -> std::path::PathBuf {
        let dir = temp_dir(tag);
        fs::write(dir.join("package.json"), "{ \"name\": \"fixture\" }\n").unwrap();
        fs::write(dir.join("README.md"), "# Fixture\n").unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/index.ts"), "export const x = 1;\n").unwrap();
        git_ok(&["init", "-q"], &dir);
        git_ok(&["add", "-A"], &dir);
        git_ok(&["-c", "user.name=antiloki", "-c", "user.email=antiloki@local", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"], &dir);
        dir
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory; ATTACH ':memory:' AS knowledge;
            CREATE TABLE cards (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT UNIQUE NOT NULL, title TEXT NOT NULL, status TEXT NOT NULL);
            CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL, situation TEXT NOT NULL, guidance TEXT NOT NULL, naive_response TEXT, tags_json TEXT NOT NULL DEFAULT '[]', source_card TEXT, created_by TEXT NOT NULL, validated INTEGER NOT NULL DEFAULT 0, confidence REAL NOT NULL DEFAULT 0.5);
            CREATE TABLE knowledge.documents (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, content_md TEXT NOT NULL);",
        ).unwrap();
        c
    }

    #[test]
    fn full_pipeline_writes_memories_and_proposes_a_card() {
        let repo = fixture_repo("full");
        let c = db();
        let root = repo.to_string_lossy().into_owned();
        let r = onboardingOrchestrate(&json!({ "repo_path": root }), &c).unwrap();
        assert_eq!(r["dry_run"], json!(false));
        assert!(r["file_count"].as_i64().unwrap() >= 3);
        assert!(r["phases"].as_array().unwrap().iter().any(|p| p["name"] == json!("scan")));
        assert!(r["phases"].as_array().unwrap().iter().any(|p| p["name"] == json!("infer")));
        assert!(r["phases"].as_array().unwrap().iter().any(|p| p["name"] == json!("write")));
        assert!(r["phases"].as_array().unwrap().iter().any(|p| p["name"] == json!("propose")));
        assert!(r["proposed_card"]["slug"].as_str().unwrap().starts_with("EPIC-ONBOARDED-"));
        let card_count = crate::db_json::query_rows(&c, "SELECT COUNT(*) AS n FROM cards", &[]).unwrap()[0]["n"].clone();
        assert_eq!(card_count, json!(1));
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn dry_run_skips_write_and_propose() {
        let repo = fixture_repo("dry");
        let c = db();
        let root = repo.to_string_lossy().into_owned();
        let r = onboardingOrchestrate(&json!({ "repo_path": root, "dry_run": true }), &c).unwrap();
        assert_eq!(r["dry_run"], json!(true));
        assert_eq!(r["memories_written"], json!(0));
        assert_eq!(r["knowledge_written"], json!(0));
        assert_eq!(r["proposed_card"], Value::Null);
        assert!(r["phases"].as_array().unwrap().iter().all(|p| p["name"] != json!("propose")));
        let mem_count = crate::db_json::query_rows(&c, "SELECT COUNT(*) AS n FROM memory.memories", &[]).unwrap()[0]["n"].clone();
        assert_eq!(mem_count, json!(0));
        let card_count = crate::db_json::query_rows(&c, "SELECT COUNT(*) AS n FROM cards", &[]).unwrap()[0]["n"].clone();
        assert_eq!(card_count, json!(0));
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn propose_starter_card_false_skips_the_propose_phase() {
        let repo = fixture_repo("nopropose");
        let c = db();
        let root = repo.to_string_lossy().into_owned();
        let r = onboardingOrchestrate(&json!({ "repo_path": root, "propose_starter_card": false }), &c).unwrap();
        assert_eq!(r["proposed_card"], Value::Null);
        assert!(r["phases"].as_array().unwrap().iter().all(|p| p["name"] != json!("propose")));
        let card_count = crate::db_json::query_rows(&c, "SELECT COUNT(*) AS n FROM cards", &[]).unwrap()[0]["n"].clone();
        assert_eq!(card_count, json!(0));
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn missing_repo_path_errs() {
        let c = db();
        assert!(onboardingOrchestrate(&json!({}), &c).is_err());
    }

    #[test]
    fn slug_uppercases_and_replaces_non_alnum_with_dashes_capped_at_24() {
        let repo = fixture_repo("slugtest-Weird_Name!!");
        let c = db();
        let root = repo.to_string_lossy().into_owned();
        let r = onboardingOrchestrate(&json!({ "repo_path": root }), &c).unwrap();
        let slug = r["proposed_card"]["slug"].as_str().unwrap();
        assert!(slug.starts_with("EPIC-ONBOARDED-"));
        let suffix = &slug["EPIC-ONBOARDED-".len()..];
        assert!(suffix.len() <= 24);
        assert!(suffix.chars().all(|c| c == '-' || c.is_ascii_digit() || c.is_ascii_uppercase()));
        let _ = fs::remove_dir_all(&repo);
    }
}
