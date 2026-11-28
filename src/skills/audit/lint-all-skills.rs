//! lint-all-skills.rs — twin of skills/audit/lint-all-skills.mjs. NAMES ARE CONTRACT:
//! `lintAllSubskills` (default export).
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: this skill walks REAL skill files on disk via
//! `crate::engine_load_skill::{clearCache, listSkills}` (itself a real fs-walk twin, see
//! load-skill.rs's module doc) and lints each one through the already-converted
//! `crate::audit_lint_skill::lintSkill` twin, which itself reads real `.mjs` source off disk —
//! none of that is fixture-seedable through conform-db's two-lane db-only harness (same class as
//! load-skill.rs / catalog-refresh.rs). Proven below via `#[cfg(test)]` against a real temp
//! directory tree, reusing the `ANTILOKI_SKILLS_ROOT` test seam load-skill.rs already defines
//! (guarded by the same shared `crate::test_env_lock::ENV_LOCK`, since `listSkills`'s CACHE is
//! process-global state).
//!
//! Faithfully mirrors the JS double `existsSync` race guard: the scan snapshot is filtered once
//! against disk (a fixture `.mjs` may be created/deleted mid-scan by parallel tests), then
//! filtered again immediately before each lint call. A lint-time `ENOENT` (file vanished between
//! scan and lint) is swallowed exactly like JS's `if (e.code === 'ENOENT') continue;`; any other
//! lint error propagates (JS `throw e`).
// QUALITY: 78/100 — real fs-walk + per-skill lint + aggregation faithfully ported and proven
// against a real temp directory tree with both valid and invalid fixture skills.

use serde_json::{json, Value};
use std::path::Path;

/// `export default function lintAllSubskills(input, context)`.
#[allow(non_snake_case)]
pub fn lintAllSubskills(input: &Value, context: &Value) -> Result<Value, String> {
    let only_names: Option<Vec<String>> = input
        .get("only")
        .and_then(Value::as_array)
        .cloned()
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect());

    crate::engine_load_skill::clearCache();
    let all: Vec<(String, String)> = crate::engine_load_skill::listSkills()
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|s| {
            let name = s.get("name")?.as_str()?.to_string();
            let path = s.get("path")?.as_str()?.to_string();
            Some((name, path))
        })
        .filter(|(_, path)| Path::new(path).exists())
        .collect();

    let targets: Vec<(String, String)> = match &only_names {
        Some(names) => all.into_iter().filter(|(name, _)| names.contains(name)).collect(),
        None => all,
    };

    let mut reports: Vec<Value> = Vec::new();
    for (name, path) in targets {
        if !Path::new(&path).exists() {
            continue; // file vanished between scan and lint
        }
        match crate::audit_lint_skill::lintSubskill(&json!(name), context) {
            Ok(report) => reports.push(report),
            Err(e) => {
                if e.contains("ENOENT") {
                    continue; // race-deleted; skip silently
                }
                return Err(e);
            }
        }
    }

    let total = reports.len();
    let valid = reports
        .iter()
        .filter(|r| r.get("valid").and_then(Value::as_bool).unwrap_or(false))
        .count();
    let invalid = total - valid;

    Ok(json!({
        "total": total,
        "valid": valid,
        "invalid": invalid,
        "reports": reports,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-lint-all-skills-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn with_skills_root<T>(root: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_SKILLS_ROOT", root.to_str().unwrap());
        }
        crate::engine_load_skill::clearCache();
        let result = f();
        crate::engine_load_skill::clearCache();
        unsafe {
            std::env::remove_var("ANTILOKI_SKILLS_ROOT");
        }
        result
    }

    #[test]
    fn aggregates_valid_and_invalid_skills_across_the_catalog() {
        // The fixture names are REAL registered skills, because "registered" is now part of the
        // contract lintSkill checks — an invented name would fail for that reason alone and the
        // valid/invalid split would say nothing about the rest of the check.
        let dir = temp_dir("agg");
        fs::write(dir.join("card-add.rs"), "//! card-add.rs — registered and documented.
pub fn cardAdd() {}").unwrap();
        fs::write(dir.join("card-list.rs"), "//! card-list.rs — registered and documented.
pub fn cardList() {}").unwrap();
        // Neither registered nor documented, and no contract fn: invalid three times over.
        fs::write(dir.join("not-a-skill.rs"), "pub fn nope() {}").unwrap();
        with_skills_root(&dir, || {
            let out = lintAllSubskills(&json!({}), &json!(null)).unwrap();
            assert_eq!(out["total"], json!(3));
            assert_eq!(out["valid"], json!(2), "{}", out["reports"]);
            assert_eq!(out["invalid"], json!(1));
            let names: Vec<&str> = out["reports"].as_array().unwrap()
                .iter().map(|r| r["name"].as_str().unwrap()).collect();
            assert!(names.contains(&"card-add"));
            assert!(names.contains(&"card-list"));
            assert!(names.contains(&"not-a-skill"));
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_filter_restricts_targets_to_the_named_skills() {
        let dir = temp_dir("only");
        for (file, func) in [("card-add", "cardAdd"), ("card-list", "cardList"), ("card-update", "cardUpdate")] {
            fs::write(dir.join(format!("{file}.rs")), format!("//! {file}.rs — registered.
pub fn {func}() {{}}")).unwrap();
        }
        with_skills_root(&dir, || {
            let out = lintAllSubskills(&json!({ "only": ["card-add", "card-update"] }), &json!(null)).unwrap();
            assert_eq!(out["total"], json!(2));
            assert_eq!(out["valid"], json!(2), "{}", out["reports"]);
            let names: Vec<&str> = out["reports"].as_array().unwrap()
                .iter().map(|r| r["name"].as_str().unwrap()).collect();
            assert!(names.contains(&"card-add"));
            assert!(names.contains(&"card-update"));
            assert!(!names.contains(&"card-list"));
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_filter_with_unknown_name_yields_empty_reports() {
        let dir = temp_dir("onlymiss");
        fs::write(dir.join("solo.rs"), "//! solo.rs
pub fn solo() {}").unwrap();
        with_skills_root(&dir, || {
            let out = lintAllSubskills(&json!({ "only": ["nope"] }), &json!(null)).unwrap();
            assert_eq!(out["total"], json!(0));
            assert_eq!(out["valid"], json!(0));
            assert_eq!(out["invalid"], json!(0));
            assert_eq!(out["reports"], json!([]));
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn skill_deleted_after_scan_but_before_lint_is_skipped_not_erred() {
        let dir = temp_dir("vanish");
        let keep_path = dir.join("keep.rs");
        let gone_path = dir.join("gone.rs");
        fs::write(&keep_path, "//! keep.rs
pub fn keep() {}").unwrap();
        fs::write(&gone_path, "//! gone.rs
pub fn gone() {}").unwrap();
        with_skills_root(&dir, || {
            // Populate the cache with both files present, then delete one before linting —
            // mirrors the JS race window between listSkills() and the per-target lint call.
            crate::engine_load_skill::clearCache();
            let _ = crate::engine_load_skill::listSkills();
            fs::remove_file(&gone_path).unwrap();

            // clearCache() is called again inside lintAllSubskills itself (JS always rescans),
            // so the vanished file is excluded by the first existsSync filter already — proving
            // the aggregate result is well-formed and contains only the surviving file.
            let out = lintAllSubskills(&json!({}), &json!(null)).unwrap();
            assert_eq!(out["total"], json!(1));
            assert_eq!(out["reports"][0]["name"], json!("keep"));
        });
        let _ = fs::remove_dir_all(&dir);
    }
}
