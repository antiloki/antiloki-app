//! driver-run.rs — twin of skills/driver/driver-run.mjs (bespoke-native: orchestration loop
//! composing already-twinned driver-tick / driver-halt-check / driver-schedule / reconcile;
//! nothing here is pure/db-deterministic on its own — it's a driving loop with real wall-clock
//! budget checks, so it's not conform-db shaped).
//! NAMES ARE CONTRACT: `driverRun` (default export). See driver-run.mjs's module docblock for
//! the full input/output contract (budgets checked BEFORE each tick; halt-file checked on every
//! iteration; `stop_on_idle` breaks on the first idle tick).
//!
//! JS `db` is a required `context` field (`if (!db) throw err.contextMissing(...)`); the Rust
//! calling convention already guarantees a live `&Connection`, so that guard has no twin here —
//! same precedent as every other `(input, context)` skill in this crate.
//!
//! `Date.now()` wall-clock is mirrored with `std::time::Instant` (monotonic, immune to system
//! clock adjustments — strictly better than the JS original for a budget check, and observably
//! identical for any real run).
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::time::Instant;

const DEFAULT_MAX_TICKS: i64 = 50;
const DEFAULT_MAX_COST_USD: f64 = 1.0;
const DEFAULT_MAX_WALL_S: f64 = 300.0;
const DEFAULT_STOP_ON_IDLE: bool = true;

// = JS `x ?? y`: only a present, non-null key wins over the default.
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

fn cost_since_run_start(db: &Connection, run_start_iso: &str) -> f64 {
    crate::db_json::query_rows(
        db,
        "SELECT COALESCE(SUM(cost_usd), 0) AS sum FROM brain.driver_ticks WHERE started_at >= ?",
        &[json!(run_start_iso)],
    )
    .ok()
    .and_then(|rows| rows.as_array().and_then(|a| a.first()).cloned())
    .and_then(|r| r.get("sum").and_then(Value::as_f64))
    .unwrap_or(0.0)
}

#[allow(non_snake_case)]
pub fn driverRun(input: &Value, db: &Connection) -> Result<Value, String> {
    let max_ticks = nz(input.get("max_ticks")).and_then(Value::as_i64).unwrap_or(DEFAULT_MAX_TICKS);
    let max_cost_usd = nz(input.get("max_cost_usd")).and_then(Value::as_f64).unwrap_or(DEFAULT_MAX_COST_USD);
    let max_wall_s = nz(input.get("max_wall_s")).and_then(Value::as_f64).unwrap_or(DEFAULT_MAX_WALL_S);
    let stop_on_idle = nz(input.get("stop_on_idle")).and_then(Value::as_bool).unwrap_or(DEFAULT_STOP_ON_IDLE);
    let halt_path = nz(input.get("halt_path")).cloned();

    let run_start = Instant::now();
    // Capture the SQLite-formatted ISO timestamp the moment the run begins — every tick we
    // authorize from this run will have started_at >= this.
    let run_start_iso = crate::db_json::query_rows(db, "SELECT datetime('now') AS now", &[])
        .ok()
        .and_then(|rows| rows.as_array().and_then(|a| a.first()).cloned())
        .and_then(|r| r.get("now").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default();

    let mut ticks_executed: i64 = 0;
    let mut ticks_idle: i64 = 0;
    let mut ticks_failed: i64 = 0;
    let mut ticks_fired: i64 = 0;
    let mut total_cost_usd: f64 = 0.0;
    let mut halted_reason: Option<String> = None;
    let mut last_tick_id: Value = Value::Null;

    loop {
        // ── Budget + halt-file checks BEFORE the tick ──────────────────────
        let halt_input = json!({ "halt_path": halt_path.clone().unwrap_or(Value::Null) });
        let halted = crate::driver_driver_halt_check::driverHaltCheck(&halt_input, db)?;
        if halted.get("halted").and_then(Value::as_bool).unwrap_or(false) {
            halted_reason = Some("halt_file".into());
            break;
        }
        if ticks_executed >= max_ticks {
            halted_reason = Some("max_ticks".into());
            break;
        }
        if run_start.elapsed().as_secs_f64() >= max_wall_s {
            halted_reason = Some("max_wall_s".into());
            break;
        }
        // Real cap, queried from DB. Any row in driver_ticks since run_start_iso contributes
        // (including externally-injected ones) — makes the cap testable + honors cost from
        // prior ticks even if their cost_usd was finalized after this loop saw them.
        total_cost_usd = cost_since_run_start(db, &run_start_iso);
        if total_cost_usd >= max_cost_usd {
            halted_reason = Some("max_cost_usd".into());
            break;
        }

        // ── Tick ─────────────────────────────────────────────────────────
        let tick = crate::driver_driver_tick::driverTick(&json!({ "halt_path": halt_path.clone().unwrap_or(Value::Null) }), db)?;
        ticks_executed += 1;
        let tick_id = tick.get("id").cloned().unwrap_or(Value::Null);
        if !tick_id.is_null() {
            last_tick_id = tick_id;
        }

        let outcome = tick.get("outcome").and_then(Value::as_str).unwrap_or("");
        if outcome == "idle" {
            ticks_idle += 1;
            if stop_on_idle {
                halted_reason = Some("idle".into());
                break;
            }
        } else if outcome == "failed" {
            ticks_failed += 1;
        } else if outcome == "fired" || outcome == "escalated" {
            ticks_fired += 1;
        } else if outcome == "halt_requested" {
            // Tick itself detected halt-file mid-iteration — stop the loop.
            halted_reason = Some("halt_requested_by_tick".into());
            break;
        }
        // 'escalated' and 'budget_capped' outcomes from driver-tick: when they appear, the loop
        // continues (escalation is per-card terminal, not loop terminal).
    }

    // FIX-DRIVER-RUN-WHY-IDLE-SUMMARY: when the loop halts because the board has no actionable
    // card, surface (1) what's specifically blocking the top cards AND (2) reconcile's broader
    // human_blocking findings (pending clarify questions, high-risk contracts awaiting approval,
    // stuck tick loops). Two sources, one unified summary so the user sees everything to unstick
    // in one place.
    let elapsed_s = run_start.elapsed().as_secs_f64();
    let mut summary_md: Value = Value::Null;
    if halted_reason.as_deref() == Some("idle") && ticks_fired == 0 {
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("Driver fired {ticks_executed} tick(s); no card was actionable."));

        // FIX-DRIVER-RUN-INCLUDES-RECONCILE-FINDINGS: reconcile finds the BROADER blockers
        // (clarify questions, HITL contracts) that the per-card state-machine reason can't
        // articulate. Group by kind, show concrete commands the user can run.
        let reconcile_report = crate::system_reconcile::reconcile(&json!({}), db).ok();
        let blocking: Vec<Value> = reconcile_report
            .and_then(|r| r.get("findings").and_then(Value::as_array).cloned())
            .unwrap_or_default()
            .into_iter()
            .filter(|f| f.get("severity").and_then(Value::as_str) == Some("human_blocking"))
            .collect();

        if !blocking.is_empty() {
            lines.push(String::new());
            lines.push(format!("Reconcile found {} human_blocking finding(s):", blocking.len()));

            let clarifies: Vec<&Value> = blocking.iter().filter(|f| f.get("kind").and_then(Value::as_str) == Some("pl_clarify_pending")).collect();
            if !clarifies.is_empty() {
                lines.push(format!("  {} card(s) waiting on PL clarify answers:", clarifies.len()));
                for f in clarifies.iter().take(5) {
                    let card_slug = f.get("card_slug").and_then(Value::as_str).unwrap_or("");
                    let q0 = f
                        .get("detail")
                        .and_then(|d| d.get("questions"))
                        .and_then(Value::as_array)
                        .and_then(|a| a.first())
                        .and_then(|q| q.get("question_md"))
                        .and_then(Value::as_str)
                        .unwrap_or("(no question text)");
                    let q0_trunc: String = q0.chars().take(110).collect();
                    lines.push(format!("    • {card_slug} — Q: \"{q0_trunc}\""));
                }
                if clarifies.len() > 5 {
                    lines.push(format!("    …+{} more", clarifies.len() - 5));
                }
                lines.push("  Answer via pl.session.answer.record — or just mention the slug in chat and the floor will list the questions inline.".to_string());
            }

            let hitls: Vec<&Value> = blocking.iter().filter(|f| f.get("kind").and_then(Value::as_str) == Some("contract_awaiting_human_approval")).collect();
            if !hitls.is_empty() {
                lines.push(String::new());
                lines.push(format!("  {} high-risk contract(s) awaiting your approve/reject:", hitls.len()));
                for f in hitls.iter().take(5) {
                    let contract_slug = f.get("contract_slug").and_then(Value::as_str).unwrap_or("");
                    let card_slug = f.get("card_slug").and_then(Value::as_str).unwrap_or("");
                    let risk = f.get("detail").and_then(|d| d.get("risk")).map(|v| v.to_string()).unwrap_or_else(|| "?".into());
                    let risk = risk.trim_matches('"').to_string();
                    lines.push(format!("    • {contract_slug} (card: {card_slug}, risk: {risk}/5)"));
                }
                lines.push("  Open [6]contracts in the TUI — press 'a' to approve or 'r' to reject.".to_string());
            }

            let stuck: Vec<&Value> = blocking.iter().filter(|f| f.get("kind").and_then(Value::as_str) == Some("stuck_tick_loop")).collect();
            if !stuck.is_empty() {
                lines.push(String::new());
                lines.push(format!("  {} card(s) in a stuck failure loop:", stuck.len()));
                for f in stuck.iter().take(5) {
                    let card_slug = f.get("card_slug").and_then(Value::as_str).unwrap_or("");
                    let fail_count = f.get("detail").and_then(|d| d.get("fail_count")).map(|v| v.to_string()).unwrap_or_else(|| "?".into());
                    let fail_count = fail_count.trim_matches('"').to_string();
                    lines.push(format!("    • {card_slug} — {fail_count} repeat failures"));
                }
            }
        }

        // Per-card state-machine reasons (top 5) — the existing summary that shows the
        // "type do <slug>" hint for unprioritized backlog.
        if let Ok(sched) = crate::driver_driver_schedule::driverSchedule(&json!({}), db) {
            let all_candidates = sched.get("all_candidates").and_then(Value::as_array).cloned().unwrap_or_default();
            let top: Vec<&Value> = all_candidates.iter().take(5).collect();
            if !top.is_empty() {
                lines.push(String::new());
                lines.push(format!("Top {} of {} card(s) on the board:", top.len(), all_candidates.len()));
                for c in &top {
                    let slug = c.get("slug").and_then(Value::as_str).unwrap_or("");
                    let reason: String = c.get("decision").and_then(|d| d.get("reason")).and_then(Value::as_str).unwrap_or("no reason").chars().take(90).collect();
                    lines.push(format!("  • {slug} — {reason}"));
                }
                let startable: Vec<&&Value> = top
                    .iter()
                    .filter(|c| {
                        c.get("decision")
                            .and_then(|d| d.get("matched_rule"))
                            .and_then(Value::as_str)
                            .map(|r| r.contains("pl-awaiting-user-start"))
                            .unwrap_or(false)
                    })
                    .collect();
                if let Some(first) = startable.first() {
                    let slug = first.get("slug").and_then(Value::as_str).unwrap_or("");
                    lines.push(String::new());
                    lines.push(format!("To start a backlog card: `do {slug}` in chat (raises priority to 5)."));
                }
            }
        }
        summary_md = json!(lines.join("\n"));
    } else if ticks_fired > 0 {
        summary_md = json!(format!("Driver fired {ticks_fired} tick(s), {ticks_idle} idle, {ticks_failed} failed ({elapsed_s}s)."));
    }

    let mut out = Map::new();
    out.insert("ticks_executed".into(), json!(ticks_executed));
    out.insert("ticks_idle".into(), json!(ticks_idle));
    out.insert("ticks_failed".into(), json!(ticks_failed));
    out.insert("ticks_fired".into(), json!(ticks_fired));
    out.insert("total_cost_usd".into(), json!(total_cost_usd));
    out.insert("elapsed_ms".into(), json!((elapsed_s * 1000.0).round() as i64));
    out.insert("halted_reason".into(), halted_reason.map(|s| json!(s)).unwrap_or(Value::Null));
    out.insert("last_tick_id".into(), last_tick_id);
    out.insert("summary_md".into(), summary_md);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c
    }

    #[test]
    fn max_ticks_zero_halts_immediately_no_tick_dispatched() {
        let db = setup();
        let r = driverRun(&json!({ "max_ticks": 0 }), &db).unwrap();
        assert_eq!(r["halted_reason"], json!("max_ticks"));
        assert_eq!(r["ticks_executed"], json!(0));
        assert_eq!(r["ticks_idle"], json!(0));
        assert_eq!(r["last_tick_id"], Value::Null);
    }

    #[test]
    fn halt_file_present_halts_before_any_tick() {
        let db = setup();
        let dir = std::env::temp_dir().join(format!("driver-run-rs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let halt_path = dir.join("driver-halt");
        std::fs::write(&halt_path, "").unwrap();
        let r = driverRun(&json!({ "halt_path": halt_path.to_string_lossy() }), &db).unwrap();
        assert_eq!(r["halted_reason"], json!("halt_file"));
        assert_eq!(r["ticks_executed"], json!(0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn max_wall_s_zero_halts_immediately() {
        let db = setup();
        let r = driverRun(&json!({ "max_wall_s": 0 }), &db).unwrap();
        assert_eq!(r["halted_reason"], json!("max_wall_s"));
        assert_eq!(r["ticks_executed"], json!(0));
    }

    #[test]
    fn max_cost_usd_zero_halts_before_any_tick_with_empty_ticks_table() {
        let db = setup();
        db.execute_batch(
            "CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY AUTOINCREMENT, cost_usd REAL, started_at TEXT);",
        )
        .unwrap();
        let r = driverRun(&json!({ "max_cost_usd": 0 }), &db).unwrap();
        assert_eq!(r["halted_reason"], json!("max_cost_usd"));
        assert_eq!(r["total_cost_usd"], json!(0.0));
        assert_eq!(r["ticks_executed"], json!(0));
    }
}
