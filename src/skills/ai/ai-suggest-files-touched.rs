//! ai-suggest-files-touched.rs — twin of skills/ai/ai-suggest-files-touched.mjs. Single default
//! export `aiSuggestFilesTouched` (async over sync — the only awaited work is `context.db` reads
//! plus a sync `git ls-files` subprocess, same async-lane convention as its own dependency).
//!
//! BESPOKE-NATIVE (not fixture/conform-db): the JS unconditionally re-shells `execSync('git
//! ls-files', {cwd: REPO_ROOT})` on EVERY call (its own copy, separate from the one inside
//! `ai-prompt-path-grounding.mjs` — the JS source literally duplicates the subprocess instead of
//! reusing the imported skill's internal helper, since that helper isn't re-exported), same
//! unconditional-shellout rule as `ai-prompt-path-grounding.rs`'s `aiPromptPathGrounding`. This
//! twin mirrors that duplication rather than reaching into the sibling module's private
//! `git_list_files`/`repo_root` (not `pub`) — it reimplements the identical REPO_ROOT computation
//! locally, exactly as the JS does.
//!
//! REPO_ROOT: JS computes `resolve(HERE, '..', '..', '..')` from this file's own
//! `import.meta.url` (`HERE` = `skills/ai`, same directory as the sibling) — identical formula,
//! identical landing spot (one level above `antiloki-core`), so `repo_root()` here is a verbatim
//! copy of `ai-prompt-path-grounding.rs`'s. Not itself a git repo in this checkout (verified there
//! already) → `git_list_files()` deterministically returns `[]` in production reality here too.
//!
//! Reused twin: calls `crate::ai_ai_prompt_path_grounding::aiPromptPathGrounding` (the sibling's
//! bespoke-native default export) exactly as the JS `import`s and `await`s it, and its pub
//! `rankCandidates` (the JS does a second dynamic `import()` of the same module purely to reach
//! `rankCandidates`/`scoreFile`, already resolved statically here as an ordinary `use`/qualified
//! call — no dynamic-import boundary to defer, both names are already `pub` on the twin).
//!
//! `min_score`/`max_paths` filter+slice: JS `ranked.filter(c=>c.score>=min_score).slice(0,
//! max_paths)` — filter first, then take, ported directly. `confidence = +...toFixed(3)`: rounded
//! to 3 decimals as an f64 (unary `+` re-parses the fixed string back to a Number).
//!
//! `compute_result(input, db, cwd)` factors out the `cwd` used for `git ls-files` so native tests
//! can exercise the full filter/confidence pipeline against a real temp-dir repo (sibling's
//! `git_list_files(cwd)` precedent) — production's `aiSuggestFilesTouched` always calls it with
//! the fixed `repo_root()`, matching the JS exactly (no such parameter there).

use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS: `const HERE = dirname(fileURLToPath(import.meta.url)); const REPO_ROOT = resolve(HERE,
// '..', '..', '..');` — verbatim copy of ai-prompt-path-grounding.rs's repo_root().
fn repo_root() -> String {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    match std::path::Path::new(manifest_dir).parent() {
        Some(p) => p.to_string_lossy().into_owned(),
        None => manifest_dir.to_string(),
    }
}

// `execSync('git ls-files', {cwd: REPO_ROOT})`, catch → `[]`. `cwd` is a plain parameter here
// (JS has none) purely so tests can drive this against a real temp-dir repo.
fn git_list_files(cwd: &str) -> Vec<String> {
    match crate::proc::cmd("git").arg("ls-files").current_dir(cwd).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .split('\n')
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

fn compute_result(input: &Value, db: &Connection, cwd: &str) -> Result<Value, String> {
    let card_slug = input.get("card_slug").cloned().unwrap_or(Value::Null);
    let keywords = input.get("keywords").cloned().unwrap_or(Value::Null);
    let max_paths: i64 = input.get("max_paths").and_then(Value::as_i64).unwrap_or(5);
    let min_score: i64 = input.get("min_score").and_then(Value::as_i64).unwrap_or(1);

    let grounding_input = json!({
        "card_slug": card_slug,
        "keywords": keywords,
        "max_paths": max_paths * 4,
    });
    let grounding = crate::ai_ai_prompt_path_grounding::aiPromptPathGrounding(&grounding_input, db)?;
    let keywords_used = grounding.get("keywords_used").cloned().unwrap_or(json!([]));

    let files = json!(git_list_files(cwd));
    let ranked = crate::ai_ai_prompt_path_grounding::rankCandidates(&files, &keywords_used, &json!(max_paths * 4));
    let ranked_arr = ranked.as_array().cloned().unwrap_or_default();
    let filtered: Vec<Value> = ranked_arr
        .into_iter()
        .filter(|c| c.get("score").and_then(Value::as_i64).unwrap_or(0) >= min_score)
        .take(max_paths.max(0) as usize)
        .collect();

    let max_possible = keywords_used.as_array().map(|a| a.len() as i64).unwrap_or(0).max(1);
    let top_score = filtered
        .first()
        .and_then(|c| c.get("score"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let confidence_raw = (top_score as f64 / max_possible as f64).min(1.0);
    let confidence = (confidence_raw * 1000.0).round() / 1000.0;

    let files_touched: Vec<Value> = filtered
        .iter()
        .map(|c| c.get("path").cloned().unwrap_or(Value::Null))
        .collect();

    let mut out = Map::new();
    out.insert("files_touched".into(), json!(files_touched));
    out.insert("confidence".into(), json!(confidence));
    out.insert("scored".into(), json!(filtered));
    out.insert("keywords_used".into(), keywords_used);
    out.insert("source".into(), json!("deterministic-path-grounding"));
    Ok(Value::Object(out))
}

/// `export default async function aiSuggestFilesTouched(input, context)`
pub fn aiSuggestFilesTouched(input: &Value, db: &Connection) -> Result<Value, String> {
    compute_result(input, db, &repo_root())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, body_json TEXT)",
        )
        .unwrap();
        c
    }

    fn temp_repo(tag: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-ai-suggest-files-touched-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        crate::proc::cmd("git").args(["init", "-q", "-b", "main"]).current_dir(&dir).output().unwrap();
        fs::create_dir_all(dir.join("skills/qa")).unwrap();
        fs::write(dir.join("skills/qa/qa-acceptance-run.mjs"), "// a").unwrap();
        fs::write(dir.join("skills/qa/qa-other.mjs"), "// b").unwrap();
        fs::write(dir.join("readme.md"), "# readme").unwrap();
        crate::proc::cmd("git").args(["add", "-A"]).current_dir(&dir).output().unwrap();
        crate::proc::cmd("git")
            .args(["-c", "user.name=antiloki", "-c", "user.email=antiloki@local", "commit", "-q", "-m", "init"])
            .current_dir(&dir)
            .output()
            .unwrap();
        dir
    }

    #[test]
    fn requires_card_slug_or_keywords_propagates_grounding_error() {
        let c = db0();
        let e = aiSuggestFilesTouched(&json!({}), &c).unwrap_err();
        assert!(e.contains("card_slug or keywords"));
    }

    #[test]
    fn card_slug_not_found_propagates_error() {
        let c = db0();
        let e = aiSuggestFilesTouched(&json!({ "card_slug": "NOPE" }), &c).unwrap_err();
        assert!(e.to_lowercase().contains("not found"));
    }

    #[test]
    fn no_matching_files_yields_empty_result_zero_confidence() {
        let c = db0();
        let r = aiSuggestFilesTouched(&json!({ "keywords": ["nothingmatches"] }), &c).unwrap();
        assert_eq!(r["files_touched"], json!([]));
        assert_eq!(r["confidence"], json!(0.0));
        assert_eq!(r["scored"], json!([]));
        assert_eq!(r["keywords_used"], json!(["nothingmatches"]));
        assert_eq!(r["source"], json!("deterministic-path-grounding"));
    }

    #[test]
    fn ranks_and_scores_real_files_by_keyword_overlap() {
        let c = db0();
        let dir = temp_repo("ranks");
        let cwd = dir.to_string_lossy().into_owned();
        let r = compute_result(&json!({ "keywords": ["qa", "acceptance"] }), &c, &cwd).unwrap();
        let touched: Vec<&str> = r["files_touched"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(touched[0], "skills/qa/qa-acceptance-run.mjs", "highest score first");
        assert!(touched.contains(&"skills/qa/qa-other.mjs"));
        assert!(!touched.contains(&"readme.md"));
        assert_eq!(r["confidence"], json!(1.0), "top score 2 / max_possible 2 keywords");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn min_score_filters_out_low_matches() {
        let c = db0();
        let dir = temp_repo("min-score");
        let cwd = dir.to_string_lossy().into_owned();
        let r = compute_result(&json!({ "keywords": ["qa", "acceptance"], "min_score": 2 }), &c, &cwd).unwrap();
        let touched: Vec<&str> = r["files_touched"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(touched, vec!["skills/qa/qa-acceptance-run.mjs"], "score-1 file excluded");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn max_paths_caps_the_result() {
        let c = db0();
        let dir = temp_repo("max-paths");
        let cwd = dir.to_string_lossy().into_owned();
        let r = compute_result(&json!({ "keywords": ["qa"], "max_paths": 1 }), &c, &cwd).unwrap();
        assert_eq!(r["files_touched"].as_array().unwrap().len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn card_slug_feeds_keywords_used_from_body_and_title() {
        let c = db0();
        let body = json!({ "problem": "fix qa acceptance check" }).to_string();
        c.execute(
            "INSERT INTO cards (slug, title, status, body_json) VALUES (?1, ?2, 'backlog', ?3)",
            rusqlite::params!["TEST-CARD", "fix qa acceptance", body],
        )
        .unwrap();
        let dir = temp_repo("card-slug");
        let cwd = dir.to_string_lossy().into_owned();
        let r = compute_result(&json!({ "card_slug": "TEST-CARD" }), &c, &cwd).unwrap();
        let used: Vec<&str> = r["keywords_used"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert!(used.contains(&"acceptance"));
        let touched: Vec<&str> = r["files_touched"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert!(touched.contains(&"skills/qa/qa-acceptance-run.mjs"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn production_repo_root_matches_sibling_twins_empty_shape() {
        // This checkout dir (one above antiloki-core / antiloki-core-rust) is not itself a git
        // repo — same documented shape as ai-prompt-path-grounding.rs's own production test.
        let c = db0();
        let r = aiSuggestFilesTouched(&json!({ "keywords": ["mjs"] }), &c).unwrap();
        assert_eq!(r["files_touched"], json!([]));
    }
}
