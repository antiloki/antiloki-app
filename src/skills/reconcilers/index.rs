//! index.rs — twin of skills/reconcilers/index.mjs. NAMES ARE CONTRACT: `listReconcilers`,
//! `clearReconcilerCache` (named exports), `reconcilersIndex` (default export).
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested. `listReconcilers()` does two things the
//! two-lane harness cannot represent:
//!   1. a REAL `readdirSync(HERE)` directory scan (filtered to `*.mjs`, excluding `index.mjs`
//!      and `*.test.mjs`) — deterministic, fs-dependent, ported below as `list_reconciler_files`
//!      and proven against a real temp directory in `#[cfg(test)]`.
//!   2. a PER-FILE `await import(pathToFileURL(...))` — genuine ESM dynamic module evaluation.
//!      This is PARITY-DEFERRED: Rust has no JS engine to execute an arbitrary `.mjs` file at
//!      runtime, so there is no faithful equivalent (same class as an aiCall/network boundary —
//!      the crossing playbook's bespoke-native category explicitly lists dynamic-import here).
//!      Everything the JS does to the RESULT of that import — the shape gate (`r.id` truthy,
//!      `typeof r.detect === 'function'`, `typeof r.fix === 'function'`, else silently `continue`;
//!      `r.safety` must be one of safe|risky|manual else throw AntilokiError(VALIDATION_ENUM)),
//!      and the final `.sort((a,b) => a.id.localeCompare(b.id))` — IS ported faithfully below as
//!      pure functions (`validate_reconciler_shape`, `sort_reconciler_ids`) operating on
//!      already-resolved module metadata, since that logic needs no JS engine at all. A caller
//!      with a real dynamic-import mechanism (e.g. embedding a JS runtime, or a future registry
//!      that reads reconciler metadata from a non-JS manifest) could compose
//!      `list_reconciler_files` + `validate_reconciler_shape` + `sort_reconciler_ids` into the
//!      full `listReconcilers()` pipeline; this twin does not attempt that composition itself.
//!
//! `clearReconcilerCache()` is a JS no-op kept for back-compat — ported as a literal no-op below.
//!
//! `sort_reconciler_ids` uses `str::cmp` (byte-order) as the `localeCompare` approximation — every
//! real reconciler id in this substrate is a plain ASCII kebab-case slug (e.g. `stuck-red-notifs`),
//! for which byte order and `localeCompare` agree (same disclosed gap as every other twin that
//! approximates `localeCompare` with `str::cmp`).
// QUALITY: 70/100 — real fs directory scan is faithful + tested; the dynamic-import module-loading
// half is a documented PARITY-DEFERRED boundary (no JS engine in Rust), so only its downstream
// pure logic (shape gate + sort) is ported and tested, not the full listReconcilers() pipeline.

use serde_json::Value;
use std::path::Path;

/// `readdirSync(HERE).filter(f => f.endsWith('.mjs')).filter(f => f !== 'index.mjs')
/// .filter(f => !f.endsWith('.test.mjs'))`. Order matches whatever the OS's `read_dir` yields
/// (same as JS `readdirSync`'s OS-dependent order — production code never relies on this
/// pre-sort order since the real registry sorts by loaded `id` afterward, a step this twin
/// documents as deferred, see module doc).
pub fn list_reconciler_files(dir: &Path) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    let mut files: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".mjs") {
            continue;
        }
        if name == "index.mjs" {
            continue;
        }
        if name.ends_with(".test.mjs") {
            continue;
        }
        files.push(name);
    }
    Ok(files)
}

/// Mirrors the per-module shape gate inside `listReconcilers()`, operating on already-resolved
/// module metadata (the dynamic `import()` that would produce it is the deferred boundary — see
/// module doc): `if (!r || typeof r !== 'object') continue;` `if (!r.id || typeof r.detect !==
/// 'function' || typeof r.fix !== 'function') continue;` then `if (!['safe','risky',
/// 'manual'].includes(r.safety)) throw AntilokiError({code:'VALIDATION_ENUM', skill:
/// 'reconcilers-index', cause: ...})`. Returns `Ok(true)` (push), `Ok(false)` (silent skip), or
/// `Err(message)` matching the JS thrown error's `.message` (`"<skill>: <cause>"`).
pub fn validate_reconciler_shape(
    id: Option<&str>,
    has_detect: bool,
    has_fix: bool,
    safety: Option<&str>,
) -> Result<bool, String> {
    let id = match id {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(false),
    };
    if !has_detect || !has_fix {
        return Ok(false);
    }
    match safety {
        Some("safe") | Some("risky") | Some("manual") => Ok(true),
        other => Err(format!(
            "reconcilers-index: reconciler {id} has invalid safety='{}' (must be safe|risky|manual)",
            other.unwrap_or("undefined")
        )),
    }
}

/// `reconcilers.sort((a, b) => a.id.localeCompare(b.id))` — see module doc for the
/// `localeCompare` → `str::cmp` approximation.
pub fn sort_reconciler_ids(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

/// `export function clearReconcilerCache() {}` — literal JS no-op, kept for back-compat.
pub fn clearReconcilerCache() -> Value {
    Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-reconcilers-index-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn list_reconciler_files_filters_index_and_test_files() {
        let dir = temp_dir("list");
        fs::write(dir.join("index.mjs"), "").unwrap();
        fs::write(dir.join("stuck-red-notifs.mjs"), "").unwrap();
        fs::write(dir.join("stuck-red-notifs.test.mjs"), "").unwrap();
        fs::write(dir.join("orphan-worktree-dirs.mjs"), "").unwrap();
        fs::write(dir.join("README.md"), "").unwrap();

        let mut files = list_reconciler_files(&dir).unwrap();
        files.sort();
        assert_eq!(
            files,
            vec!["orphan-worktree-dirs.mjs".to_string(), "stuck-red-notifs.mjs".to_string()]
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_reconciler_files_empty_dir_returns_empty() {
        let dir = temp_dir("empty");
        assert_eq!(list_reconciler_files(&dir).unwrap(), Vec::<String>::new());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_reconciler_files_missing_dir_errs() {
        let mut dir = std::env::temp_dir();
        dir.push("antiloki-reconcilers-index-rs-does-not-exist");
        let _ = fs::remove_dir_all(&dir);
        assert!(list_reconciler_files(&dir).is_err());
    }

    #[test]
    fn validate_reconciler_shape_skips_missing_id_or_missing_methods() {
        assert_eq!(validate_reconciler_shape(None, true, true, Some("safe")), Ok(false));
        assert_eq!(validate_reconciler_shape(Some(""), true, true, Some("safe")), Ok(false));
        assert_eq!(validate_reconciler_shape(Some("x"), false, true, Some("safe")), Ok(false));
        assert_eq!(validate_reconciler_shape(Some("x"), true, false, Some("safe")), Ok(false));
    }

    #[test]
    fn validate_reconciler_shape_accepts_known_safety_values() {
        for s in ["safe", "risky", "manual"] {
            assert_eq!(validate_reconciler_shape(Some("x"), true, true, Some(s)), Ok(true));
        }
    }

    #[test]
    fn validate_reconciler_shape_throws_on_invalid_safety() {
        let e = validate_reconciler_shape(Some("stuck-red-notifs"), true, true, Some("dangerous"))
            .unwrap_err();
        assert_eq!(
            e,
            "reconcilers-index: reconciler stuck-red-notifs has invalid safety='dangerous' (must be safe|risky|manual)"
        );
    }

    #[test]
    fn validate_reconciler_shape_throws_on_missing_safety() {
        let e = validate_reconciler_shape(Some("x"), true, true, None).unwrap_err();
        assert_eq!(
            e,
            "reconcilers-index: reconciler x has invalid safety='undefined' (must be safe|risky|manual)"
        );
    }

    #[test]
    fn sort_reconciler_ids_matches_locale_compare_for_ascii_kebab_ids() {
        let ids = vec![
            "stuck-red-notifs".to_string(),
            "brain-envelope-bloat".to_string(),
            "card-status-mirrors-work".to_string(),
        ];
        assert_eq!(
            sort_reconciler_ids(ids),
            vec![
                "brain-envelope-bloat".to_string(),
                "card-status-mirrors-work".to_string(),
                "stuck-red-notifs".to_string(),
            ]
        );
    }

    #[test]
    fn clear_reconciler_cache_is_a_noop() {
        assert_eq!(clearReconcilerCache(), Value::Null);
    }
}
