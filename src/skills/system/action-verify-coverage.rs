//! action-verify-coverage.rs — twin of skills/system/action-verify-coverage.mjs (db READ lane). NAMES
//! ARE CONTRACT: `actionVerifyCoverage` (named default). Verification-spec coverage, grouped by agent.
// QUALITY: 82/100 — overall/critical/per-agent coverage %, driver-critical set, agent sort + Math.round
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;

const DRIVER_CRITICAL: [&str; 22] = [
    "pl.session.start", "pl.session.advance.clarify", "pl.session.advance.research",
    "pl.session.advance.decompose", "pl.session.finalize", "pl.session.approve",
    "card.gate.pl.ratify", "contract.draft.auto", "contract.gate.tl.sign", "tl.sandbox.create",
    "tl.refine.microtasks", "git-manager.implement.code", "qa.gate.contract.verify", "qa.acceptance.run",
    "man.card.review", "keeper.card.consolidate", "keeper.journey.append",
    "board-keeper.card.add", "board-keeper.card.update", "board-keeper.card.status.change",
    "system.work.start", "system.driver.tick",
];

fn round_pct(covered: i64, total: i64) -> i64 {
    if total == 0 { 0 } else { ((covered as f64 / total as f64) * 100.0).round() as i64 }
}

pub fn actionVerifyCoverage(input: &Value, db: &Connection) -> Result<Value, String> {
    let _ = input;
    let rows = crate::db_json::query_rows(db,
        "SELECT name, owner_agent, verification_json IS NOT NULL AS has_spec FROM actions ORDER BY owner_agent, name", &[])?;
    let critical: std::collections::HashSet<&str> = DRIVER_CRITICAL.iter().copied().collect();

    let mut by_agent: HashMap<String, (i64, i64)> = HashMap::new();
    let (mut total, mut covered) = (0i64, 0i64);
    let (mut crit_total, mut crit_covered) = (0i64, 0i64);
    let mut crit_uncovered: Vec<Value> = Vec::new();

    for r in rows.as_array().unwrap() {
        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let agent = r.get("owner_agent").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let has = r.get("has_spec").and_then(|v| v.as_i64()).unwrap_or(0) != 0;
        total += 1;
        if has { covered += 1; }
        if critical.contains(name) {
            crit_total += 1;
            if has { crit_covered += 1; } else { crit_uncovered.push(json!(name)); }
        }
        let e = by_agent.entry(agent).or_insert((0, 0));
        e.0 += 1;
        if has { e.1 += 1; }
    }

    let mut agents: Vec<&String> = by_agent.keys().collect();
    agents.sort();
    let per_agent: Vec<Value> = agents.iter().map(|a| {
        let (t, c) = by_agent[*a];
        json!({ "agent": a, "total": t, "covered": c, "coverage_pct": round_pct(c, t) })
    }).collect();

    Ok(json!({
        "overall": { "total": total, "covered": covered, "coverage_pct": round_pct(covered, total) },
        "driver_critical": { "total": crit_total, "covered": crit_covered, "coverage_pct": round_pct(crit_covered, crit_total), "uncovered": crit_uncovered },
        "per_agent": per_agent,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coverage_grouped() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE actions (name TEXT, owner_agent TEXT, verification_json TEXT); \
          INSERT INTO actions VALUES ('system.work.start','sys','{}'),('a.b','sys',NULL),('c.d','pl','{}');").unwrap();
        let r = actionVerifyCoverage(&json!({}), &c).unwrap();
        assert_eq!(r["overall"]["total"], json!(3));
        assert_eq!(r["overall"]["covered"], json!(2));
        assert_eq!(r["driver_critical"]["total"], json!(1)); // system.work.start
        assert_eq!(r["per_agent"][0]["agent"], json!("pl")); // sorted
    }
}
