//! design-rebuild.rs — twin of skills/codebase/design-rebuild.mjs. NAMES ARE CONTRACT:
//! `designRebuild` (default, db). FEAT-DESIGN-SYSTEM: re-harvests the design palette + component
//! inventory (+ best-effort HTML/CSS snapshots) on demand for the Design screen's "rebuild"
//! button — the same three deterministic harvesters the atlas rebuild fires.
//!
//! Cross-module calls (all pre-existing Rust twins, per the crossing playbook's cross-module-call
//! convention): `codebase_code_schema::ensureCodeSchema`,
//! `codebase_design_tokens_extract::designTokensExtract`,
//! `codebase_design_components_extract::designComponentsExtract`,
//! `codebase_design_snapshot_build::designSnapshotBuild`.
//!
//! `ensureCodeSchema` never throws in this Rust twin (its DDL exec swallows errors via `let _ =
//! db.execute_batch(...)`) — unlike JS, where referencing the unattached `code.` schema throws and
//! is caught. Since designTokensExtract/designComponentsExtract read straight off the tables this
//! call creates (they carry no ensureCodeSchema of their own), WHICH prefix actually lands matters
//! for db_after: check whether a `code` schema is attached (`pragma_database_list`) and pick the
//! prefix accordingly — the same branch outcome the JS try/catch produces (attached -> 'code.'
//! succeeds; not attached -> the JS catch falls back to '').
//!
//! `context.db` is always injected by the db lane in production, so the JS `if (!context?.db)
//! throw err.contextMissing('design-rebuild', 'db')` guard is unreachable here (mirrors every
//! sibling twin's convention, playbook rule 9 — guard-throws are not exercised).
//!
//! The JS awaits `designTokensExtract`/`designComponentsExtract` bare (no try/catch) — a real
//! throw there propagates out of `designRebuild` itself, mirrored here via `?`. Only
//! `designSnapshotBuild` is wrapped in its own try/catch (`{ok:false, error:String(e?.message ??
//! e)}` on throw); neither Rust twin ever returns `Err` on any transcribed path, so that fallback
//! is unreachable on the fixture but kept faithful to the JS control flow.
use rusqlite::Connection;
use serde_json::{json, Value};

fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

fn code_schema_attached(db: &Connection) -> bool {
    db.query_row("SELECT 1 FROM pragma_database_list WHERE name = 'code'", [], |_| Ok(()))
        .is_ok()
}

pub fn designRebuild(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin_v = nz(input.get("origin")).cloned().unwrap_or_else(|| json!("atlas"));

    let prefix = if code_schema_attached(db) { "code." } else { "" };
    crate::codebase_code_schema::ensureCodeSchema(db, &json!(prefix));

    let tokens = crate::codebase_design_tokens_extract::designTokensExtract(
        &json!({ "all": true, "origin": origin_v.clone() }),
        db,
    )?;
    let components = crate::codebase_design_components_extract::designComponentsExtract(
        &json!({ "all": true, "origin": origin_v.clone() }),
        db,
    )?;

    let snapshots = match crate::codebase_design_snapshot_build::designSnapshotBuild(
        &json!({ "origin": origin_v.clone() }),
        db,
    ) {
        Ok(v) => v,
        Err(e) => json!({ "ok": false, "error": e }),
    };

    Ok(json!({
        "ok": true,
        "origin": origin_v,
        "tokens": tokens,
        "components": components,
        "snapshots": snapshots
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn rebuild_default_origin_with_no_atlas_files_still_ok() {
        let c = db();
        // No "code" schema attached, and no code_onboard_jobs/code_nodes rows — each harvester's
        // internal ensureCodeSchema('code.') no-ops (swallow-all), then its own SELECTs return
        // empty (no attached "code" schema -> unqualified reads see nothing seeded), so every
        // stage settles on its own zero-work happy path.
        let r = designRebuild(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["origin"], json!("atlas"));
        assert!(r.get("tokens").is_some());
        assert!(r.get("components").is_some());
        assert!(r.get("snapshots").is_some());
    }

    #[test]
    fn rebuild_honors_explicit_origin() {
        let c = db();
        let r = designRebuild(&json!({ "origin": "blueprint:x@1" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["origin"], json!("blueprint:x@1"));
    }
}
