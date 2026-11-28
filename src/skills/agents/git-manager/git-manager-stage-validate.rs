//! git-manager-stage-validate.rs — twin of skills/agents/git-manager/git-manager-stage-validate.mjs
//! (db READ + real git subprocess). NAMES ARE CONTRACT: `gitManagerStageValidate` (default).
//! Refuses stages that touch files outside the contract's `files_touched_json`: extra staged
//! files (not in contract) are SCOPE_DRIFT; files in contract but not staged are INCOMPLETE.
//!
//! GIT SUBPROCESS BRANCH — DISCLOSED UNTESTED (playbook lesson 5's flag-skippable pattern, same
//! shape as constitution-estimate.rs's `git_diff_paths`): the JS `spawnSync('git', ['diff',
//! '--cached', '--name-only'], { cwd })` call fires ONLY when the contract's `files_touched_json`
//! resolves to a non-empty array — an empty/absent contract short-circuits BEFORE the git call
//! (JS: `if (!Array.isArray(allowed) || allowed.length === 0) return {...}`). This twin ports the
//! subprocess branch for real (`git_diff_cached_names`, mirroring `spawnSync`'s cwd + stdout/
//! stderr/status handling via `std::process::Command`), but the fixture only exercises the
//! empty-`files_touched` short-circuit — real git staging state is not deterministically fixture-
//! controllable the way a committed testdata dir is. The JS behavior test (git-manager-stage-
//! validate.test.mjs) DOES exercise this branch end-to-end against a real temp git repo, so the
//! subprocess wiring itself is verified there even though conform-db never runs it.
//!
//! `context.db` missing → JS throws `err.contextMissing('git-manager-stage-validate', 'db')`. Not
//! modeled: `db: &Connection` is a required, non-nullable reference — same "guard-throws are not
//! exercised" convention every sibling db-lane twin uses.
// QUALITY: 80/100 — real subprocess branch ported+doc'd, JS-side end-to-end covered; conform-db fixture only proves the git-free short-circuit path

use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

/// The `spawnSync('git', ['diff', '--cached', '--name-only'], { cwd })` call — see module doc:
/// real subprocess, disclosed UNTESTED by the conform-db fixture (git-state dependent,
/// flag-skippable per playbook lesson 5; exercised instead by the JS behavior test).
fn git_diff_cached_names(cwd: &str) -> Result<Vec<String>, String> {
    let output = crate::proc::cmd("git")
        .arg("diff")
        .arg("--cached")
        .arg("--name-only")
        .current_dir(cwd)
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            Ok(text.split('\n').map(|l| l.trim().to_string()).filter(|s| !s.is_empty()).collect())
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            // JS: `(diff.stderr || '').slice(0, 200)` — UTF-16 code units; `.chars().take(200)`
            // is the established BMP-safe approximation used elsewhere in this codebase.
            let capped: String = stderr.chars().take(200).collect();
            Err(format!("git-manager-stage-validate: git diff --cached failed: {capped}"))
        }
        // spawn failure (git not found, etc.) — JS's `diff.status` would be `null` (`!== 0`),
        // `diff.stderr` undefined → `(undefined || '').slice(0,200)` = ''.
        Err(_) => Err("git-manager-stage-validate: git diff --cached failed: ".into()),
    }
}

pub fn gitManagerStageValidate(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `if (!db) throw err.contextMissing(...)` — unreachable here; db: &Connection is
    // required, non-nullable.
    let card_slug = match input.get("card_slug").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        // JS `if (!card_slug) throw err.required(...)` is a falsy-only gate (a truthy non-string
        // card_slug would still be forwarded); narrowed to "non-empty string" per the same
        // disclosed convention card-run-preflight.rs uses — real callers always pass a string
        // card_slug (a TEXT column value).
        None => return Err("git-manager-stage-validate: card_slug is required".into()),
    };

    let contract = crate::db_json::query_rows(
        db,
        "SELECT slug, files_touched_json FROM contracts WHERE card_slug = ? ORDER BY id DESC LIMIT 1",
        &[json!(card_slug.clone())],
    )?
    .as_array()
    .and_then(|a| a.first())
    .cloned();
    let contract = match contract {
        Some(c) => c,
        None => return Err(format!("git-manager-stage-validate: no contract found for card '{card_slug}'")),
    };

    let contract_slug = contract.get("slug").cloned().unwrap_or(Value::Null);
    let files_touched_json = contract.get("files_touched_json").cloned().unwrap_or(Value::Null);
    let allowed_v = crate::engine_json_utils::loadJson(&files_touched_json, &json!([]));

    let allowed: Vec<Value> = match allowed_v.as_array() {
        Some(a) if !a.is_empty() => a.clone(),
        _ => {
            return Ok(json!({
                "ok": true,
                "contract_slug": contract_slug,
                "staged": [],
                "extra": [],
                "missing": [],
                "note": "contract has empty files_touched — nothing to validate against",
            }));
        }
    };

    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or_default();
    let staged = git_diff_cached_names(root)?;

    // JS `new Set(allowed)` may in theory contain non-string entries (raw JSON array contents);
    // a non-string entry can never equal a staged filename (all strings), so filtering the set
    // to strings-only doesn't change membership results — see git-manager-merge-gate.rs-style
    // callers for the same "allowed set of strings" shape.
    let allowed_strs: HashSet<&str> = allowed.iter().filter_map(Value::as_str).collect();
    let staged_set: HashSet<&str> = staged.iter().map(String::as_str).collect();

    let extra: Vec<Value> = staged.iter().filter(|f| !allowed_strs.contains(f.as_str())).map(|f| json!(f)).collect();
    // `allowed.filter((f) => !stagedSet.has(f))` — a non-string `f` can never be in stagedSet
    // (all strings), so it is always kept as "missing", matching `map_or(true, ...)` below.
    let missing: Vec<Value> = allowed
        .iter()
        .filter(|f| f.as_str().map_or(true, |s| !staged_set.contains(s)))
        .cloned()
        .collect();
    let finding_count = (extra.len() + missing.len()) as i64;

    Ok(json!({
        "ok": extra.is_empty(),
        "contract_slug": contract_slug,
        "staged": staged,
        "allowed": allowed,
        "extra": extra,
        "missing": missing,
        "finding_count": finding_count,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_contract(card_slug: &str, contract_slug: &str, files_touched_json: Option<&str>) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, card_slug TEXT, files_touched_json TEXT)",
        )
        .unwrap();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, files_touched_json) VALUES (?1, ?2, ?3)",
            rusqlite::params![contract_slug, card_slug, files_touched_json],
        )
        .unwrap();
        c
    }

    #[test]
    fn missing_card_slug_errors() {
        let c = db_with_contract("CARD-A", "CT-1", Some("[]"));
        assert_eq!(
            gitManagerStageValidate(&json!({}), &c),
            Err("git-manager-stage-validate: card_slug is required".to_string())
        );
    }

    #[test]
    fn no_contract_found_errors() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, card_slug TEXT, files_touched_json TEXT)").unwrap();
        assert_eq!(
            gitManagerStageValidate(&json!({ "card_slug": "GHOST" }), &c),
            Err("git-manager-stage-validate: no contract found for card 'GHOST'".to_string())
        );
    }

    #[test]
    fn empty_files_touched_short_circuits_with_note() {
        let c = db_with_contract("CARD-A", "CT-1", Some("[]"));
        let r = gitManagerStageValidate(&json!({ "card_slug": "CARD-A" }), &c).unwrap();
        assert_eq!(
            r,
            json!({
                "ok": true, "contract_slug": "CT-1", "staged": [], "extra": [], "missing": [],
                "note": "contract has empty files_touched — nothing to validate against",
            })
        );
    }

    #[test]
    fn null_files_touched_json_also_short_circuits() {
        let c = db_with_contract("CARD-A", "CT-1", None);
        let r = gitManagerStageValidate(&json!({ "card_slug": "CARD-A" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["note"], json!("contract has empty files_touched — nothing to validate against"));
    }
}
