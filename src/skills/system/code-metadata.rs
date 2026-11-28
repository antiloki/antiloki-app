//! system/code-metadata.rs — twin of skills/system/code-metadata.mjs. PARITY-DEFERRED (aiCall
//! boundary): `codeMetadata` resolves + reads one tracked workspace file via the already-converted
//! `crate::system_code_file::codeFile` twin, sha256-gates against `brain.code_file_metadata`
//! (cache hit unless `recompute:true`), and on a miss calls `crate::ai_ai_call::aiCall` — the
//! deferred network-AI stub that always returns `Err` (see ai-call.rs's module doc). The JS
//! `await aiCall(...)` is unguarded (no try/catch), so a real Err there propagates straight out of
//! `codeMetadata` exactly like `?` does here: `foldOutput` and the `brain.code_file_metadata`
//! INSERT/UPDATE ON CONFLICT are ported faithfully but — like book-run.rs's `fac_call` past
//! `aiCall` — unreachable past the deferred stub in this file's own tests; correctness there is by
//! inspection + `foldOutput` being independently unit-testable against a hand-built `output_kv`.
//!
//! NAMES ARE CONTRACT: default export `codeMetadata`; named exports `sha256`, `foldOutput` (both
//! ported as `pub fn` for the same reasons — trivial pure helpers, testable in isolation).
//!
//! Reused cross-module twins (not reimplemented): `crate::system_code_file::codeFile` (path
//! resolve + read, traversal/tracked guards) and `crate::engine_workspace_root::getActiveWorkspace`
//! (active workspace row → `workspace_id`, `0` when none registered — JS `?.id ?? 0`).
// QUALITY: bespoke-native (deferred AI boundary past the deterministic cache-gate surface — see
// module doc; not conform-db fixtured, same class as book-run.rs)

use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// `function sha256(s)` — `createHash('sha256').update(s, 'utf8').digest('hex')`.
pub fn sha256(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hex::encode(hasher.finalize())
}

/// `function foldOutput(output_kv)` — folds ai-call's output_kv rows into
/// `{summary, score, suggestions[]}`. `suggestions` arrives as one row per array element.
pub fn foldOutput(output_kv: &Value) -> Value {
    let empty: Vec<Value> = Vec::new();
    let rows = output_kv.as_array().unwrap_or(&empty);
    let mut summary = String::new();
    let mut score: Value = Value::Null;
    let mut suggestions: Vec<Value> = Vec::new();
    for r in rows {
        let field_name = r.get("field_name").and_then(Value::as_str).unwrap_or("");
        if field_name == "summary" {
            summary = r.get("value_text").and_then(Value::as_str).unwrap_or("").to_string();
        } else if field_name == "score" {
            score = r.get("value_number").cloned().filter(|v| !v.is_null()).unwrap_or(Value::Null);
        } else if field_name == "suggestions" {
            if let Some(t) = r.get("value_text").filter(|v| !v.is_null()) {
                suggestions.push(t.clone());
            }
        }
    }
    json!({ "summary": summary, "score": score, "suggestions": suggestions })
}

fn read_cache(db: &Connection, workspace_id: i64, path: &str, sha: &str) -> Option<Value> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT summary, score, suggestions_json, ai_call_id FROM brain.code_file_metadata WHERE workspace_id = ? AND path = ? AND sha256 = ?",
        &[json!(workspace_id), json!(path), json!(sha)],
    ).ok()?;
    let row = rows.as_array().and_then(|a| a.first())?;
    let suggestions_json = row.get("suggestions_json").and_then(Value::as_str).unwrap_or("[]");
    let suggestions = serde_json::from_str::<Value>(suggestions_json).ok()
        .filter(Value::is_array)
        .unwrap_or_else(|| json!([]));
    Some(json!({
        "summary": row.get("summary").and_then(Value::as_str).unwrap_or(""),
        "score": row.get("score").cloned().unwrap_or(Value::Null),
        "suggestions": suggestions,
        "ai_call_id": row.get("ai_call_id").cloned().unwrap_or(Value::Null),
    }))
}

/// `export default async function codeMetadata(input, context)` — `context.provider_fn` is
/// folded straight into the (always-`Err`) `aiCall` input like dev-exec-sandbox.rs's precedent;
/// since the deferred stub never reads its input, the Rust signature drops the unused param and
/// takes `(input, db)` like every other twin.
pub fn codeMetadata(input: &Value, db: &Connection) -> Result<Value, String> {
    let empty = json!({});
    let input_obj = if input.is_object() { input } else { &empty };
    let path = match input_obj.get("path").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => return Err("code-metadata: path is required".to_string()),
    };
    let recompute = input_obj.get("recompute").and_then(Value::as_bool).unwrap_or(false);

    let file_res = crate::system_code_file::codeFile(&json!({ "path": path }), db)?;
    let rel = file_res.get("path").and_then(Value::as_str).unwrap_or_default().to_string();
    let content = file_res.get("content").and_then(Value::as_str).unwrap_or("").to_string();
    let sha = sha256(&content);
    let active = crate::engine_workspace_root::getActiveWorkspace(db);
    let workspace_id = active.get("id").and_then(Value::as_i64).unwrap_or(0);

    if !recompute {
        if let Some(cached) = read_cache(db, workspace_id, &rel, &sha) {
            let mut out = json!({ "path": rel, "sha256": sha });
            for (k, v) in cached.as_object().unwrap() { out[k] = v.clone(); }
            out["cached"] = json!(true);
            return Ok(out);
        }
    }

    let ai_input = json!({
        "prompt_name": "code.file.metadata",
        "input_kv": [{ "key": "path", "value": rel }, { "key": "content", "value": content }],
        "bypass_cache": recompute,
    });
    let out_res = crate::ai_ai_call::aiCall(&ai_input, db)?;
    let output_kv = out_res.get("output_kv").cloned().unwrap_or(json!([]));
    let folded = foldOutput(&output_kv);
    let summary = folded.get("summary").and_then(Value::as_str).unwrap_or("").to_string();
    let score = folded.get("score").cloned().unwrap_or(Value::Null);
    let suggestions = folded.get("suggestions").cloned().unwrap_or(json!([]));
    let ai_call_id = out_res.get("ai_call_id").cloned().unwrap_or(Value::Null);

    let score_i = score.as_f64().filter(|f| f.is_finite()).map(|f| f.round() as i64);
    crate::db_json::run_write(
        db,
        "INSERT INTO brain.code_file_metadata (workspace_id, path, sha256, summary, score, suggestions_json, ai_call_id) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT (workspace_id, path, sha256) DO UPDATE SET summary = excluded.summary, score = excluded.score, suggestions_json = excluded.suggestions_json, ai_call_id = excluded.ai_call_id, created_at = datetime('now')",
        &[
            json!(workspace_id), json!(rel), json!(sha), json!(summary),
            match score_i { Some(n) => json!(n), None => Value::Null },
            json!(suggestions.to_string()), ai_call_id.clone(),
        ],
    )?;

    Ok(json!({
        "path": rel, "sha256": sha, "summary": summary, "score": score,
        "suggestions": suggestions, "ai_call_id": ai_call_id, "cached": false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    // ── temp-git-repo + workspace fixture, mirrors system_code_file.rs's own test helpers ──
    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-code-metadata-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run_git_ok(args: &[&str], cwd: &std::path::Path) {
        let out = Command::new("git").args(args).current_dir(cwd).output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }

    fn make_temp_git_repo(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        run_git_ok(&["init", "-q"], &dir);
        run_git_ok(&["config", "user.email", "test@test"], &dir);
        run_git_ok(&["config", "user.name", "test"], &dir);
        dir
    }

    fn db_with_workspace(root: &std::path::Path) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00'); \
             CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY AUTOINCREMENT, worktree_path TEXT)",
        ).unwrap();
        c.execute(
            "INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', ?1, 1)",
            rusqlite::params![root.to_string_lossy().into_owned()],
        ).unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.code_file_metadata (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               workspace_id INTEGER NOT NULL,
               path TEXT NOT NULL,
               sha256 TEXT NOT NULL,
               summary TEXT,
               score INTEGER,
               suggestions_json TEXT,
               ai_call_id INTEGER,
               created_at TEXT NOT NULL DEFAULT (datetime('now')),
               UNIQUE (workspace_id, path, sha256)
             );",
        ).unwrap();
        c
    }

    #[test]
    fn sha256_of_empty_string_is_well_known_sha256_empty() {
        assert_eq!(sha256(""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn fold_output_reads_summary_score_and_collects_suggestions() {
        let kv = json!([
            { "field_name": "summary", "value_text": "A test file." },
            { "field_name": "score", "value_number": 73 },
            { "field_name": "suggestions", "value_text": "Add tests." },
            { "field_name": "suggestions", "value_text": "Shorten function." },
        ]);
        let folded = foldOutput(&kv);
        assert_eq!(folded["summary"], "A test file.");
        assert_eq!(folded["score"], 73.0);
        assert_eq!(folded["suggestions"], json!(["Add tests.", "Shorten function."]));
    }

    #[test]
    fn fold_output_empty_kv_yields_empty_summary_null_score_empty_suggestions() {
        let folded = foldOutput(&json!([]));
        assert_eq!(folded["summary"], "");
        assert!(folded["score"].is_null());
        assert_eq!(folded["suggestions"], json!([]));
    }

    #[test]
    fn code_metadata_requires_path() {
        let root = make_temp_git_repo("requires-path");
        let db = db_with_workspace(&root);
        let err = codeMetadata(&json!({}), &db).unwrap_err();
        assert_eq!(err, "code-metadata: path is required");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn code_metadata_untracked_file_propagates_code_file_not_found_error() {
        let root = make_temp_git_repo("untracked");
        fs::write(root.join("ignored.txt"), "not tracked").unwrap();
        let db = db_with_workspace(&root);
        let err = codeMetadata(&json!({ "path": "ignored.txt" }), &db).unwrap_err();
        assert_eq!(err, "code-file: tracked file 'ignored.txt' not found");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn code_metadata_returns_cache_hit_without_reaching_ai_call() {
        let root = make_temp_git_repo("cache-hit");
        fs::write(root.join("app.ts"), "console.log('hi');").unwrap();
        run_git_ok(&["add", "app.ts"], &root);
        let db = db_with_workspace(&root);
        let sha = sha256("console.log('hi');");
        db.execute(
            "INSERT INTO brain.code_file_metadata (workspace_id, path, sha256, summary, score, suggestions_json, ai_call_id) VALUES (1, 'app.ts', ?1, 'cached summary', 88, '[\"s1\"]', 42)",
            [&sha],
        ).unwrap();

        let out = codeMetadata(&json!({ "path": "app.ts" }), &db).unwrap();
        assert_eq!(out["cached"], true);
        assert_eq!(out["summary"], "cached summary");
        assert_eq!(out["score"], 88);
        assert_eq!(out["suggestions"], json!(["s1"]));
        assert_eq!(out["ai_call_id"], 42);
        assert_eq!(out["sha256"], sha);
        assert_eq!(out["path"], "app.ts");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn code_metadata_cache_miss_propagates_deferred_ai_call_err() {
        let root = make_temp_git_repo("cache-miss");
        fs::write(root.join("app.ts"), "console.log('hi');").unwrap();
        run_git_ok(&["add", "app.ts"], &root);
        let db = db_with_workspace(&root);

        // No cache row seeded: recompute path is forced to reach crate::ai_ai_call::aiCall,
        // which always returns Err — mirrors JS's unguarded `await aiCall(...)` propagating.
        let out = codeMetadata(&json!({ "path": "app.ts" }), &db);
        assert!(out.is_err(), "expected deferred aiCall Err to propagate, got {out:?}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn code_metadata_recompute_true_skips_cache_even_when_present() {
        let root = make_temp_git_repo("recompute");
        fs::write(root.join("app.ts"), "console.log('hi');").unwrap();
        run_git_ok(&["add", "app.ts"], &root);
        let db = db_with_workspace(&root);
        let sha = sha256("console.log('hi');");
        db.execute(
            "INSERT INTO brain.code_file_metadata (workspace_id, path, sha256, summary, score, suggestions_json, ai_call_id) VALUES (1, 'app.ts', ?1, 'cached summary', 88, '[\"s1\"]', 42)",
            [&sha],
        ).unwrap();

        // recompute:true bypasses the cache read entirely, so it must reach (and fail at) the
        // deferred aiCall boundary instead of returning the cached row.
        let out = codeMetadata(&json!({ "path": "app.ts", "recompute": true }), &db);
        assert!(out.is_err(), "expected recompute:true to bypass cache and hit deferred aiCall, got {out:?}");
        let _ = fs::remove_dir_all(&root);
    }
}
