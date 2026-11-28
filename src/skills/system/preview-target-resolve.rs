//! preview-target-resolve.rs — NEW skill (Rust-only). NAMES ARE CONTRACT: `previewTargetResolve`.
//!
//! Resolve what a preview/run request is pointed AT, and on what runtime.
//!
//! WHY IT EXISTS. antiloki-BE's pr.controller answered this with five separate raw queries
//! inline: a cockpit pane's worktree, the active workspace root, a contract's worktree (twice),
//! and three cost_settings keys for the execution runtime. Only a skill touches the databases.
//!
//! One skill rather than five, because they are one QUESTION asked three ways. The caller has a
//! pane, a root request, or a contract slug, and in every case wants the same answer: which
//! directory, under which key, on which runtime. Splitting it back into per-table lookups would
//! leave the branch logic — including which error the operator sees — in the customer layer.
//!
//! NON-THROWING. A missing worktree is a normal operator state (the PR merged, the pane has no
//! worktree provisioned yet), not a fault, so it comes back as `{ok:false, error}` for the caller
//! to render. Only a malformed request is an Err.
// QUALITY: 88/100 — three-branch target resolution, legacy-table tolerance, runtime passthrough
use rusqlite::Connection;
use serde_json::{json, Value};

fn cell(db: &Connection, sql: &str, params: &[Value], col: &str) -> Option<String> {
    // Legacy workspaces predate some of these tables; a missing table is "no answer", not a
    // failure (the BE wrapped two of these in try/catch for exactly that reason).
    let rows = crate::db_json::query_rows(db, sql, params).ok()?;
    rows.as_array()?
        .first()?
        .get(col)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// The three cost_settings keys that describe where code actually RUNS.
///
/// When the runtime is docker the dev server has to run INSIDE the container — its Node, its
/// port 3000, which the container already publishes. Running it on the host instead collides with
/// that publish and surfaces as "Something is already running on port 3000".
fn runtime_of(db: &Connection) -> Value {
    let kv = |k: &str| cell(db, "SELECT value FROM cost_settings WHERE key = ?", &[json!(k)], "value");
    json!({
        "runtime": kv("ws_runtime"),
        "container": kv("ws_runtime_container"),
        "image": kv("ws_runtime_image"),
    })
}

pub fn previewTargetResolve(input: &Value, db: &Connection) -> Result<Value, String> {
    let ref_slug = input.get("ref_slug").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let pane_id = input
        .get("pane_id")
        .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .filter(|n| n.is_finite() && *n > 0.0)
        .map(|n| n as i64);
    let want_root = input.get("target").and_then(|v| v.as_str()) == Some("root");

    let fail = |msg: String| Ok(json!({ "ok": false, "error": msg }));

    // Precedence is pane → root → contract, matching the BE's if/else-if chain: a pane_id is the
    // most specific thing a caller can name, and a bare ref_slug the least.
    let (key, worktree) = if let Some(id) = pane_id {
        let wt = cell(db, "SELECT worktree_path FROM cockpit_panes WHERE id = ?", &[json!(id)], "worktree_path");
        if wt.is_none() {
            return fail(format!("pane {id} has no worktree provisioned"));
        }
        (format!("pane:{id}"), wt)
    } else if want_root {
        let wt = cell(db, "SELECT root_path FROM workspaces WHERE is_active = 1", &[], "root_path");
        if wt.is_none() {
            return fail("no active workspace root".to_string());
        }
        ("root".to_string(), wt)
    } else if !ref_slug.is_empty() {
        let wt = cell(db, "SELECT worktree_path FROM contracts WHERE slug = ?", &[json!(ref_slug)], "worktree_path");
        if wt.is_none() {
            // The operator-facing wording matters: a merged or cleaned PR is the usual cause, and
            // saying so stops it reading as a bug.
            return fail("no worktree on this PR (merged or cleaned)".to_string());
        }
        (ref_slug.clone(), wt)
    } else {
        return fail("ref_slug, pane_id, or target:\"root\" required".to_string());
    };

    Ok(json!({
        "ok": true,
        "key": key,
        "worktree_path": worktree,
        "runtime": runtime_of(db),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY, worktree_path TEXT);
             CREATE TABLE workspaces (root_path TEXT, is_active INTEGER);
             CREATE TABLE contracts (slug TEXT, worktree_path TEXT);
             CREATE TABLE cost_settings (key TEXT, value TEXT);
             INSERT INTO cockpit_panes VALUES (1,'C:/wt/pane-1'),(2,NULL);
             INSERT INTO workspaces VALUES ('C:/dev/root',1),('C:/dev/other',0);
             INSERT INTO contracts VALUES ('CT-1','C:/wt/ct-1'),('CT-MERGED',NULL);
             INSERT INTO cost_settings VALUES ('ws_runtime','docker'),('ws_runtime_container','app'),
                                              ('ws_runtime_image','node:20-bookworm');",
        )
        .unwrap();
        c
    }

    #[test]
    fn a_pane_resolves_to_its_worktree_under_a_pane_key() {
        let r = previewTargetResolve(&json!({ "pane_id": 1 }), &db()).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["key"], json!("pane:1"));
        assert_eq!(r["worktree_path"], json!("C:/wt/pane-1"));
    }

    #[test]
    fn target_root_resolves_to_the_ACTIVE_workspace_only() {
        let r = previewTargetResolve(&json!({ "target": "root" }), &db()).unwrap();
        assert_eq!(r["key"], json!("root"));
        assert_eq!(r["worktree_path"], json!("C:/dev/root"), "the inactive workspace must not win");
    }

    #[test]
    fn a_ref_slug_resolves_to_the_contracts_worktree() {
        let r = previewTargetResolve(&json!({ "ref_slug": "CT-1" }), &db()).unwrap();
        assert_eq!(r["key"], json!("CT-1"));
        assert_eq!(r["worktree_path"], json!("C:/wt/ct-1"));
    }

    #[test]
    fn a_pane_id_WINS_over_a_ref_slug() {
        // Matches the BE's if/else-if precedence: the more specific target wins.
        let r = previewTargetResolve(&json!({ "pane_id": 1, "ref_slug": "CT-1", "target": "root" }), &db()).unwrap();
        assert_eq!(r["key"], json!("pane:1"));
    }

    #[test]
    fn every_missing_target_is_ok_false_with_the_operators_wording() {
        let c = db();
        let cases = [
            (json!({ "pane_id": 2 }), "has no worktree provisioned"),
            (json!({ "ref_slug": "CT-MERGED" }), "merged or cleaned"),
            (json!({ "ref_slug": "NO-SUCH" }), "merged or cleaned"),
            (json!({}), "required"),
        ];
        for (input, expect) in cases {
            let r = previewTargetResolve(&input, &c).unwrap();
            assert_eq!(r["ok"], json!(false), "{input} should not resolve");
            assert!(r["error"].as_str().unwrap().contains(expect), "{input} -> {}", r["error"]);
        }
    }

    #[test]
    fn no_active_workspace_reports_that_rather_than_picking_one() {
        let c = db();
        c.execute("UPDATE workspaces SET is_active = 0", []).unwrap();
        let r = previewTargetResolve(&json!({ "target": "root" }), &c).unwrap();
        assert_eq!(r["error"], json!("no active workspace root"));
    }

    #[test]
    fn the_execution_runtime_rides_along() {
        // The caller needs this in the SAME answer: deciding host-vs-container after the fact is
        // what produced "Something is already running on port 3000".
        let r = previewTargetResolve(&json!({ "ref_slug": "CT-1" }), &db()).unwrap();
        assert_eq!(r["runtime"], json!({ "runtime": "docker", "container": "app", "image": "node:20-bookworm" }));
    }

    #[test]
    fn a_LEGACY_db_missing_these_tables_degrades_instead_of_failing() {
        // Older workspaces predate cockpit_panes and cost_settings; a missing table is "no
        // answer", not a 500.
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE contracts (slug TEXT, worktree_path TEXT);
                         INSERT INTO contracts VALUES ('CT-1','C:/wt/ct-1');").unwrap();
        let r = previewTargetResolve(&json!({ "ref_slug": "CT-1" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["runtime"]["runtime"], Value::Null);
        assert_eq!(previewTargetResolve(&json!({ "pane_id": 1 }), &c).unwrap()["ok"], json!(false));
    }

    #[test]
    fn a_pane_id_arriving_as_a_STRING_is_accepted() {
        // It comes off an HTTP body, where a number is often a string.
        assert_eq!(previewTargetResolve(&json!({ "pane_id": "1" }), &db()).unwrap()["key"], json!("pane:1"));
    }
}
