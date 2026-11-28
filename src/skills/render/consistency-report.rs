//! consistency-report.rs — twin of skills/render/consistency-report.mjs. NAMES ARE CONTRACT:
//! `consistencyReport` (default).
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: this is a thin dispatcher over
//! `crate::audit_consistency_scan::consistencyScan` (already-converted twin), which itself walks
//! REAL directories/files on disk and is not fixture-seedable through conform-db's two-lane
//! db-only harness. The markdown-assembly logic in THIS file (group by severity → group by
//! dimension → format) is pure/deterministic and is exercised directly against synthetic
//! findings/summary structures, plus one end-to-end integration test that reuses
//! consistency-scan.rs's own `base_db()` / `scaffold_repo()` / `with_substrate_root()` test
//! fixtures to run the real scan and render its real output.
//!
//! `if (!db) throw err.contextMissing('consistency-report', 'db')` is unreachable here (`db:
//! &Connection` is a required parameter, same convention as dev-build-gate.rs / board-summary.rs)
//! and is not ported.
// QUALITY: 90/100 — pure formatting logic fully covered incl. severity/dimension grouping order and empty-state banner; one real end-to-end pass through consistencyScan

use rusqlite::Connection;
use serde_json::Value;

fn sev_emoji(sev: &str) -> &'static str {
    match sev {
        "critical" => "\u{1f534}",
        "warning" => "\u{1f7e1}",
        "info" => "\u{1f7e2}",
        _ => "",
    }
}

const SEV_ORDER: [&str; 3] = ["critical", "warning", "info"];

fn render_report(findings: &[Value], summary: &Value) -> String {
    let critical = summary.get("critical").cloned().unwrap_or(serde_json::json!(0));
    let warning = summary.get("warning").cloned().unwrap_or(serde_json::json!(0));
    let info = summary.get("info").cloned().unwrap_or(serde_json::json!(0));
    let total = summary.get("total").cloned().unwrap_or(serde_json::json!(0));

    let mut out: Vec<String> = Vec::new();
    out.push("# Consistency report".to_string());
    out.push(String::new());
    out.push(format!(
        "**{} critical \u{b7} {} warning \u{b7} {} info** (total {})",
        critical, warning, info, total
    ));
    out.push(String::new());

    let crit_zero = critical.as_i64() == Some(0);
    let warn_zero = warning.as_i64() == Some(0);
    if crit_zero && warn_zero {
        out.push("\u{2705} No critical or warning findings.".to_string());
        out.push(String::new());
    }

    for sev in SEV_ORDER {
        let group: Vec<&Value> = findings
            .iter()
            .filter(|f| f.get("severity").and_then(Value::as_str) == Some(sev))
            .collect();
        if group.is_empty() {
            continue;
        }
        out.push(format!("## {} {} ({})", sev_emoji(sev), sev.to_uppercase(), group.len()));
        out.push(String::new());

        // Sub-group by dimension, preserving first-seen dimension order (JS: plain object insertion order).
        let mut dim_order: Vec<String> = Vec::new();
        let mut by_dim: std::collections::HashMap<String, Vec<&Value>> = std::collections::HashMap::new();
        for g in &group {
            let dim = g.get("dimension").and_then(Value::as_str).unwrap_or("").to_string();
            if !by_dim.contains_key(&dim) {
                dim_order.push(dim.clone());
            }
            by_dim.entry(dim).or_default().push(g);
        }
        for dim in &dim_order {
            let items = &by_dim[dim];
            out.push(format!("### {} ({})", dim, items.len()));
            out.push(String::new());
            for f in items {
                let message = f.get("message").and_then(Value::as_str).unwrap_or("");
                out.push(format!("- {message}"));
                let evidence = f.get("evidence");
                let has_evidence = evidence
                    .and_then(Value::as_object)
                    .map(|o| !o.is_empty())
                    .unwrap_or(false);
                if has_evidence {
                    out.push("  ```json".to_string());
                    out.push(format!("  {}", evidence.unwrap()));
                    out.push("  ```".to_string());
                }
            }
            out.push(String::new());
        }
    }

    out.join("\n")
}

/// `export default function consistencyReport(input, context)`.
pub fn consistencyReport(_input: &Value, db: &Connection) -> Result<Value, String> {
    let scan = crate::audit_consistency_scan::consistencyScan(&Value::Null, db)?;
    let findings = scan.get("findings").and_then(Value::as_array).cloned().unwrap_or_default();
    let summary = scan.get("summary").cloned().unwrap_or(serde_json::json!({}));
    Ok(Value::String(render_report(&findings, &summary)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-consistency-report-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn with_substrate_root<T>(root: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", root.to_str().unwrap()); }
        let result = f();
        unsafe { std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT"); }
        result
    }

    fn base_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE actions (name TEXT PRIMARY KEY, steps_json TEXT NOT NULL);
             CREATE TABLE agents (name TEXT PRIMARY KEY);
             CREATE TABLE agent_actions (id INTEGER PRIMARY KEY AUTOINCREMENT, agent_name TEXT, action_name TEXT, revoked_at TEXT);
             CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT NOT NULL);
             CREATE TABLE sacred_timeline (
               id INTEGER PRIMARY KEY, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT,
               entity_id TEXT, before_json TEXT, after_json TEXT, payload TEXT, prev_print TEXT, digital_print TEXT
             );
             CREATE TABLE schema_migrations (name TEXT PRIMARY KEY);
             ATTACH ':memory:' AS laws;
             CREATE TABLE laws.laws (slug TEXT PRIMARY KEY, enforcement TEXT, enforcement_hook TEXT, trigger_sql TEXT, superseded_by TEXT);
             ATTACH ':memory:' AS memory;
             CREATE TABLE memory.skills (name TEXT, path TEXT, sha256 TEXT);
             ATTACH ':memory:' AS knowledge;
             CREATE TABLE knowledge.documents (id INTEGER PRIMARY KEY, vault_path TEXT, sha256 TEXT, superseded_by TEXT);",
        )
        .unwrap();
        c
    }

    fn scaffold_repo(root: &Path) {
        fs::create_dir_all(root.join("antiloki-core").join("migrations")).unwrap();
        fs::create_dir_all(root.join("antiloki-core").join("skills").join("audit")).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("AGENTS.md"), "no laws here").unwrap();
        fs::write(root.join("CLAUDE.md"), "@AGENTS.md\n").unwrap();
    }

    #[test]
    fn render_report_empty_state_shows_no_findings_banner() {
        let summary = json!({ "critical": 0, "warning": 0, "info": 3, "total": 3 });
        let findings = vec![json!({ "severity": "info", "dimension": "D1", "message": "ok", "evidence": {} })];
        let s = render_report(&findings, &summary);
        assert!(s.starts_with("# Consistency report\n\n**0 critical \u{b7} 0 warning \u{b7} 3 info** (total 3)\n\n\u{2705} No critical or warning findings.\n\n"));
        assert!(s.contains("## \u{1f7e2} INFO (1)"));
        assert!(s.contains("### D1 (1)"));
        assert!(s.contains("- ok"));
    }

    #[test]
    fn render_report_with_critical_omits_banner_and_shows_evidence() {
        let summary = json!({ "critical": 1, "warning": 0, "info": 0, "total": 1 });
        let findings = vec![json!({
            "severity": "critical", "dimension": "AGENT_SKILL_INTEGRITY",
            "message": "grant 5: agent='x' action='y' — one or both missing",
            "evidence": { "id": 5, "agent_name": "x" }
        })];
        let s = render_report(&findings, &summary);
        assert!(!s.contains("No critical or warning findings"));
        assert!(s.contains("## \u{1f534} CRITICAL (1)"));
        assert!(s.contains("### AGENT_SKILL_INTEGRITY (1)"));
        assert!(s.contains("- grant 5: agent='x' action='y' \u{2014} one or both missing"));
        assert!(s.contains("  ```json"));
        // serde_json's `preserve_order` feature (see Cargo.toml) keeps the json! literal's
        // insertion order (id, then agent_name) — same as JSON.stringify on a JS object literal.
        assert!(s.contains("  {\"id\":5,\"agent_name\":\"x\"}"));
        assert!(s.contains("  ```"));
    }

    #[test]
    fn render_report_groups_by_dimension_preserving_first_seen_order() {
        let summary = json!({ "critical": 0, "warning": 3, "info": 0, "total": 3 });
        let findings = vec![
            json!({ "severity": "warning", "dimension": "B_DIM", "message": "m1", "evidence": {} }),
            json!({ "severity": "warning", "dimension": "A_DIM", "message": "m2", "evidence": {} }),
            json!({ "severity": "warning", "dimension": "B_DIM", "message": "m3", "evidence": {} }),
        ];
        let s = render_report(&findings, &summary);
        let b_pos = s.find("### B_DIM (2)").unwrap();
        let a_pos = s.find("### A_DIM (1)").unwrap();
        assert!(b_pos < a_pos, "B_DIM seen first must render first: {s}");
    }

    #[test]
    fn render_report_skips_evidence_block_when_empty_object() {
        let summary = json!({ "critical": 0, "warning": 0, "info": 1, "total": 1 });
        let findings = vec![json!({ "severity": "info", "dimension": "D", "message": "m", "evidence": {} })];
        let s = render_report(&findings, &summary);
        assert!(!s.contains("```json"));
    }

    #[test]
    fn consistency_report_end_to_end_over_real_scan() {
        let dir = temp_dir("e2e");
        scaffold_repo(&dir);
        let c = base_db();
        let s = with_substrate_root(&dir, || consistencyReport(&json!({}), &c).unwrap());
        let s = s.as_str().unwrap();
        assert!(s.starts_with("# Consistency report\n"));
        assert!(s.contains("CHAIN_INTEGRITY"));
        let _ = fs::remove_dir_all(&dir);
    }
}
