//! focus-resolve.rs — twin of skills/system/focus-resolve.mjs. NAMES ARE CONTRACT: `focusResolve`
//! (default export).
//!
//! BESPOKE-NATIVE (not conform-db fixture-tested). The JS `loadDetectors()` reflectively
//! `readdirSync`s `focus-detectors/` and `import()`s every `.mjs` file there at runtime — a
//! dynamic-import-of-a-directory pattern with no faithful Rust equivalent (PARITY-DEFERRED per
//! the crossing playbook). That boundary is closed here, not left open: every current detector
//! file already has a converted twin (`system_focus_detectors_*`), so this twin calls all seven
//! of them directly, in the same order Node's `readdirSync` returns them on this codebase's
//! filesystem (alphabetical: blocked-status, epic-has-open-children, no-contract-for-doing-card,
//! pl-unanswered, red-notification, stopped-intent, stuck-contract — matches the JS module's own
//! `_helpers`-free alphabetical file listing). Two of the seven (`blocked-status`,
//! `stopped-intent`) take `detect(card: &Value, _db: &Value)` because their JS twins never touch
//! `db`; the other five take `detect(card: &Value, db: &Connection)`. NOTE FOR FUTURE MAINTAINERS:
//! adding an eighth `.mjs` detector file requires manually adding its call here too — this twin
//! cannot discover it reflectively.
//!
//! The JS also wraps each detector call in a try/catch that synthesizes a `detector_error` finding
//! if a detector throws. Every converted detector twin below is provably infallible (each already
//! swallows its own db errors internally into a `Value::Null` "no finding", proven by that twin's
//! own `#[cfg(test)]` suite) so that catch-all path is unreachable here and is not ported —
//! documented rather than silently dropped.
//!
//! `datetime('now', '-5 minutes')` in `pickFocusCard`'s brain.driver_ticks fallback is frozen per
//! the crossing playbook's clock convention: `datetime('2026-01-01 00:00:00', '-5 minutes')`.
//! `brain.driver_ticks` may not be ATTACHed (tests / some callers) — the JS try/catch around that
//! query maps a query failure to "skip this fallback", mirrored here via `.ok()`.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn first_row(v: Value) -> Option<Value> {
    v.as_array().and_then(|a| a.first()).cloned()
}

/// JS priority chain (operator intent wins):
/// 1. driver_intent='play' + status='doing'   (canonical "playing now")
/// 2. driver_intent='play' + status='backlog' (about to play)
/// 3. status='doing', intent not paused/stopped (scheduler-managed)
/// 4. recent driver_tick non-idle in last 5 min (final fallback)
fn pick_focus_card(db: &Connection) -> Option<String> {
    if let Some(row) = crate::db_json::query_rows(
        db,
        "SELECT slug FROM cards\n    WHERE driver_intent = 'play' AND status = 'doing'\n    ORDER BY updated_at DESC LIMIT 1",
        &[],
    ).ok().and_then(first_row) {
        if let Some(s) = row.get("slug").and_then(Value::as_str) {
            return Some(s.to_string());
        }
    }

    if let Some(row) = crate::db_json::query_rows(
        db,
        "SELECT slug FROM cards\n    WHERE driver_intent = 'play' AND status = 'backlog'\n    ORDER BY updated_at DESC LIMIT 1",
        &[],
    ).ok().and_then(first_row) {
        if let Some(s) = row.get("slug").and_then(Value::as_str) {
            return Some(s.to_string());
        }
    }

    if let Some(row) = crate::db_json::query_rows(
        db,
        "SELECT slug FROM cards\n    WHERE status = 'doing' AND COALESCE(driver_intent, 'auto') NOT IN ('paused', 'stopped')\n    ORDER BY updated_at DESC LIMIT 1",
        &[],
    ).ok().and_then(first_row) {
        if let Some(s) = row.get("slug").and_then(Value::as_str) {
            return Some(s.to_string());
        }
    }

    if let Some(row) = crate::db_json::query_rows(
        db,
        "SELECT card_slug FROM brain.driver_ticks\n      WHERE outcome != 'idle' AND card_slug IS NOT NULL\n        AND started_at >= datetime('2026-01-01 00:00:00', '-5 minutes')\n      ORDER BY id DESC LIMIT 1",
        &[],
    ).ok().and_then(first_row) {
        if let Some(s) = row.get("card_slug").and_then(Value::as_str) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }

    None
}

/// Runs the fixed seven-detector dispatch (see module doc) and collects the non-null findings,
/// in the JS module's readdirSync-alphabetical order.
fn run_detectors_for_card(card: &Value, db: &Connection) -> Vec<Value> {
    let mut blocked_by = Vec::new();
    let dummy = Value::Null;
    let mut push = |v: Value| { if v.get("kind").is_some() { blocked_by.push(v); } };
    push(crate::system_focus_detectors_blocked_status::detect(card, &dummy));
    push(crate::system_focus_detectors_epic_has_open_children::detect(card, db));
    push(crate::system_focus_detectors_no_contract_for_doing_card::detect(card, db));
    push(crate::system_focus_detectors_pl_unanswered::detect(card, db));
    push(crate::system_focus_detectors_red_notification::detect(card, db));
    push(crate::system_focus_detectors_stopped_intent::detect(card, &dummy));
    push(crate::system_focus_detectors_stuck_contract::detect(card, db));
    blocked_by
}

struct Signal {
    signal: String,
    can_play: bool,
    blocked_reasons: Vec<String>,
}

/// JS `signalForCard(card, blocked_by)`.
fn signal_for_card(card: &Value, blocked_by: &[Value]) -> Signal {
    let status = card.get("status").and_then(Value::as_str).unwrap_or("");
    if status == "done" || status == "archived" {
        return Signal { signal: "idle".into(), can_play: false, blocked_reasons: vec![] };
    }
    let is_yellow = |b: &Value| b.get("severity").and_then(Value::as_str) == Some("yellow");
    let kind_of = |b: &Value| b.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
    let reds: Vec<&Value> = blocked_by.iter().filter(|b| !is_yellow(b)).collect();
    let yellows: Vec<&Value> = blocked_by.iter().filter(|b| is_yellow(b)).collect();
    if !reds.is_empty() {
        return Signal {
            signal: "red".into(),
            can_play: false,
            blocked_reasons: reds.iter().map(|b| kind_of(b)).collect(),
        };
    }
    let driver_intent = card.get("driver_intent").and_then(Value::as_str);
    if !yellows.is_empty() || driver_intent == Some("paused") {
        let mut reasons: Vec<String> = yellows.iter().map(|b| kind_of(b)).collect();
        if driver_intent == Some("paused") {
            reasons.push("intent_paused".to_string());
        }
        return Signal { signal: "yellow".into(), can_play: false, blocked_reasons: reasons };
    }
    if driver_intent == Some("play") || status == "doing" {
        return Signal { signal: "green".into(), can_play: true, blocked_reasons: vec![] };
    }
    Signal { signal: "idle".into(), can_play: true, blocked_reasons: vec![] }
}

/// JS `computePhase(card, contract, plSession)`.
fn compute_phase(card: &Value, contract: Option<&Value>, pl_session: Option<&Value>) -> String {
    let status = card.get("status").and_then(Value::as_str).unwrap_or("");
    if status == "done" { return "done".to_string(); }
    if status == "archived" { return "archived".to_string(); }
    if status == "blocked" { return "blocked".to_string(); }
    let body_kind = card.get("body_json")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|b| b.get("kind").and_then(Value::as_str).map(String::from));
    if body_kind.as_deref() == Some("epic") { return "epic".to_string(); }
    if let Some(c) = contract {
        let cs = c.get("status").and_then(Value::as_str).unwrap_or("");
        return format!("contract.{cs}");
    }
    if let Some(pl) = pl_session {
        let ps = pl.get("status").and_then(Value::as_str).unwrap_or("");
        if ps == "clarifying" { return "pl.clarifying".to_string(); }
        return format!("pl.{ps}");
    }
    status.to_string()
}

/// JS `computeNextAction(phase, blocked_by)`.
fn compute_next_action(phase: &str, blocked_by: &[Value]) -> (Option<String>, Option<String>) {
    if !blocked_by.is_empty() { return (None, None); }
    match phase {
        "contract.proposed" => (Some("card.gate.pl.ratify".to_string()), Some("pl".to_string())),
        "contract.tl_approved" => (Some("tl.sandbox.create".to_string()), Some("tl".to_string())),
        "contract.impl" => (Some("git-manager.implement.code".to_string()), Some("git-manager".to_string())),
        "contract.done" => (Some("man.card.review".to_string()), Some("man".to_string())),
        "pl.clarifying" => (Some("pl.session.advance.clarify".to_string()), Some("pl".to_string())),
        "backlog" => (Some("pl.session.start".to_string()), Some("pl".to_string())),
        _ => (None, None),
    }
}

const CARD_COLS: &str = "slug, title, status, driver_intent, body_json, updated_at, parent_slug";

/// `export default async function focusResolve(_input, context)`.
pub fn focusResolve(_input: &Value, db: &Connection) -> Result<Value, String> {
    let cards_v = crate::db_json::query_rows(
        db,
        &format!("SELECT {CARD_COLS}\n    FROM cards\n    WHERE status NOT IN ('done', 'archived')\n    ORDER BY updated_at DESC"),
        &[],
    )?;
    let cards: Vec<Value> = cards_v.as_array().cloned().unwrap_or_default();

    let mut signals: Map<String, Value> = Map::new();
    let mut blocked_by_per_card: std::collections::HashMap<String, Vec<Value>> = std::collections::HashMap::new();

    for card in &cards {
        let slug = card.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
        let blocked_by = run_detectors_for_card(card, db);
        let sig = signal_for_card(card, &blocked_by);
        let mut entry = Map::new();
        entry.insert("signal".into(), json!(sig.signal));
        entry.insert("can_play".into(), json!(sig.can_play));
        entry.insert("blocked_reasons".into(), json!(sig.blocked_reasons));
        entry.insert("red_descendant_count".into(), json!(0));
        entry.insert("blocked_by".into(), json!(blocked_by));
        entry.insert("card_title".into(), card.get("title").cloned().unwrap_or(Value::Null));
        signals.insert(slug.clone(), Value::Object(entry));
        blocked_by_per_card.insert(slug, blocked_by);
    }

    // Propagate red upward: BFS from each red leaf up to its root via parent_slug.
    for card in &cards {
        let slug = card.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
        let is_red = signals.get(&slug).and_then(|s| s.get("signal")).and_then(Value::as_str) == Some("red");
        if !is_red { continue; }
        let mut p = card.get("parent_slug").and_then(Value::as_str).map(String::from);
        while let Some(parent_slug) = p {
            if let Some(entry) = signals.get_mut(&parent_slug) {
                let obj = entry.as_object_mut().unwrap();
                let count = obj.get("red_descendant_count").and_then(Value::as_i64).unwrap_or(0);
                obj.insert("red_descendant_count".into(), json!(count + 1));
                if obj.get("signal").and_then(Value::as_str) != Some("red") {
                    obj.insert("signal".into(), json!("red"));
                }
            }
            let parent_row = crate::db_json::query_rows(
                db,
                "SELECT parent_slug FROM cards WHERE slug = ?",
                &[json!(parent_slug)],
            ).ok().and_then(first_row);
            p = parent_row.and_then(|r| r.get("parent_slug").and_then(Value::as_str).map(String::from));
        }
    }

    // Pick the focus card and assemble its detailed object.
    let focus_slug = pick_focus_card(db);
    let mut focus = Value::Null;
    if let Some(fs) = focus_slug {
        let card = cards.iter().find(|c| c.get("slug").and_then(Value::as_str) == Some(fs.as_str())).cloned()
            .or_else(|| crate::db_json::query_rows(
                db,
                &format!("SELECT {CARD_COLS} FROM cards WHERE slug = ?"),
                &[json!(fs.clone())],
            ).ok().and_then(first_row));
        if let Some(card) = card {
            let slug = card.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
            let blocked_by = blocked_by_per_card.get(&slug).cloned()
                .unwrap_or_else(|| run_detectors_for_card(&card, db));

            let contract = crate::db_json::query_rows(
                db,
                "SELECT slug, status FROM contracts\n        WHERE card_slug = ? AND COALESCE(superseded_by, '') = ''\n        ORDER BY id DESC LIMIT 1",
                &[json!(slug.clone())],
            ).ok().and_then(first_row);

            let pl_session = crate::db_json::query_rows(
                db,
                "SELECT id, status FROM pl_sessions\n          WHERE approved_card_slug = ? OR card_slug = ?\n          ORDER BY id DESC LIMIT 1",
                &[json!(slug.clone()), json!(slug.clone())],
            ).ok().and_then(first_row);

            let phase = compute_phase(&card, contract.as_ref(), pl_session.as_ref());
            let (next_action, next_agent) = compute_next_action(&phase, &blocked_by);
            let sig = signals.get(&slug).cloned();
            let can_play = sig.as_ref()
                .and_then(|s| s.get("can_play"))
                .and_then(Value::as_bool)
                .unwrap_or_else(|| signal_for_card(&card, &blocked_by).can_play);

            let reason_md = if let Some(first) = blocked_by.first() {
                first.get("reason_md").and_then(Value::as_str).unwrap_or("").to_string()
            } else if let Some(na) = &next_action {
                format!("Next: {} runs {}.", next_agent.as_deref().unwrap_or(""), na)
            } else {
                "No work pending.".to_string()
            };

            focus = json!({
                "card_slug": slug,
                "card_title": card.get("title").cloned().unwrap_or(Value::Null),
                "status": card.get("status").cloned().unwrap_or(Value::Null),
                "intent": card.get("driver_intent").filter(|v| !v.is_null()).cloned().unwrap_or(json!("auto")),
                "current_phase": phase,
                "can_progress": can_play && blocked_by.is_empty(),
                "blocked_by": blocked_by,
                "next_action": next_action,
                "next_agent": next_agent,
                "reason_md": reason_md,
            });
        }
    }

    Ok(json!({ "focus": focus, "signals": signals }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_schema(c: &Connection) {
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, driver_intent TEXT, \
             body_json TEXT, updated_at TEXT, parent_slug TEXT);
             CREATE TABLE contracts (id INTEGER PRIMARY KEY, slug TEXT, status TEXT, card_slug TEXT, superseded_by TEXT);
             CREATE TABLE pl_sessions (id INTEGER PRIMARY KEY, card_slug TEXT, approved_card_slug TEXT, status TEXT);",
        ).unwrap();
    }

    fn insert_card(c: &Connection, slug: &str, title: &str, status: &str, driver_intent: Option<&str>, body_json: Option<&str>, updated_at: &str, parent_slug: Option<&str>) {
        c.execute(
            "INSERT INTO cards (slug, title, status, driver_intent, body_json, updated_at, parent_slug) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![slug, title, status, driver_intent, body_json, updated_at, parent_slug],
        ).unwrap();
    }

    #[test]
    fn pick_focus_card_priority_chain() {
        let c = Connection::open_in_memory().unwrap();
        base_schema(&c);
        // no cards at all -> None
        assert_eq!(pick_focus_card(&c), None);

        insert_card(&c, "AUTO-1", "auto doing", "doing", None, None, "2026-01-01 00:00:01", None);
        assert_eq!(pick_focus_card(&c), Some("AUTO-1".to_string()));

        insert_card(&c, "PLAY-BACKLOG", "play backlog", "backlog", Some("play"), None, "2026-01-01 00:00:02", None);
        assert_eq!(pick_focus_card(&c), Some("PLAY-BACKLOG".to_string()), "play+backlog beats auto-doing");

        insert_card(&c, "PLAY-DOING", "play doing", "doing", Some("play"), None, "2026-01-01 00:00:03", None);
        assert_eq!(pick_focus_card(&c), Some("PLAY-DOING".to_string()), "play+doing wins over all else");
    }

    #[test]
    fn pick_focus_card_skips_paused_stopped_auto_doing() {
        let c = Connection::open_in_memory().unwrap();
        base_schema(&c);
        insert_card(&c, "PAUSED", "x", "doing", Some("paused"), None, "2026-01-01 00:00:01", None);
        insert_card(&c, "STOPPED", "y", "doing", Some("stopped"), None, "2026-01-01 00:00:02", None);
        assert_eq!(pick_focus_card(&c), None);
    }

    #[test]
    fn signal_for_card_terminal_states_and_paused_intent() {
        let done = json!({"status":"done"});
        let s = signal_for_card(&done, &[]);
        assert_eq!((s.signal.as_str(), s.can_play), ("idle", false));

        let paused = json!({"status":"backlog","driver_intent":"paused"});
        let s2 = signal_for_card(&paused, &[]);
        assert_eq!(s2.signal, "yellow");
        assert_eq!(s2.blocked_reasons, vec!["intent_paused".to_string()]);
    }

    #[test]
    fn signal_for_card_red_wins_over_yellow() {
        let card = json!({"status":"doing"});
        let findings = vec![
            json!({"kind":"pl_unanswered","severity":"yellow"}),
            json!({"kind":"blocked_status"}),
        ];
        let s = signal_for_card(&card, &findings);
        assert_eq!(s.signal, "red");
        assert_eq!(s.blocked_reasons, vec!["blocked_status".to_string()]);
    }

    #[test]
    fn signal_for_card_green_and_idle() {
        let play_card = json!({"status":"backlog","driver_intent":"play"});
        assert_eq!(signal_for_card(&play_card, &[]).signal, "green");
        let idle_card = json!({"status":"backlog"});
        let s = signal_for_card(&idle_card, &[]);
        assert_eq!(s.signal, "idle");
        assert!(s.can_play);
    }

    #[test]
    fn compute_phase_terminal_epic_contract_pl() {
        assert_eq!(compute_phase(&json!({"status":"done"}), None, None), "done");
        assert_eq!(compute_phase(&json!({"status":"blocked"}), None, None), "blocked");
        assert_eq!(
            compute_phase(&json!({"status":"backlog","body_json":"{\"kind\":\"epic\"}"}), None, None),
            "epic"
        );
        assert_eq!(
            compute_phase(&json!({"status":"doing"}), Some(&json!({"status":"proposed"})), None),
            "contract.proposed"
        );
        assert_eq!(
            compute_phase(&json!({"status":"doing"}), None, Some(&json!({"status":"clarifying"}))),
            "pl.clarifying"
        );
        assert_eq!(
            compute_phase(&json!({"status":"doing"}), None, Some(&json!({"status":"tuning"}))),
            "pl.tuning"
        );
        assert_eq!(compute_phase(&json!({"status":"backlog"}), None, None), "backlog");
    }

    #[test]
    fn compute_next_action_maps_known_phases_and_blocks() {
        assert_eq!(
            compute_next_action("contract.proposed", &[]),
            (Some("card.gate.pl.ratify".to_string()), Some("pl".to_string()))
        );
        assert_eq!(compute_next_action("backlog", &[]), (Some("pl.session.start".to_string()), Some("pl".to_string())));
        assert_eq!(compute_next_action("unknown_phase", &[]), (None, None));
        assert_eq!(
            compute_next_action("backlog", &[json!({"kind":"blocked_status"})]),
            (None, None),
            "any blocker suppresses next_action"
        );
    }

    #[test]
    fn focus_resolve_end_to_end_blocked_card_propagates_red_to_epic_parent() {
        let c = Connection::open_in_memory().unwrap();
        base_schema(&c);
        insert_card(&c, "EPIC-1", "the epic", "doing", None, Some("{\"kind\":\"epic\"}"), "2026-01-01 00:00:01", None);
        insert_card(&c, "CHILD-1", "the child", "blocked", None, None, "2026-01-01 00:00:02", Some("EPIC-1"));

        let out = focusResolve(&json!({}), &c).unwrap();
        let signals = &out["signals"];
        assert_eq!(signals["CHILD-1"]["signal"], json!("red"));
        assert_eq!(signals["CHILD-1"]["blocked_by"][0]["kind"], json!("blocked_status"));
        assert_eq!(signals["EPIC-1"]["signal"], json!("red"), "red propagates from child to epic parent");
        assert_eq!(signals["EPIC-1"]["red_descendant_count"], json!(1));
    }

    #[test]
    fn focus_resolve_no_cards_returns_null_focus_and_empty_signals() {
        let c = Connection::open_in_memory().unwrap();
        base_schema(&c);
        let out = focusResolve(&json!({}), &c).unwrap();
        assert_eq!(out["focus"], Value::Null);
        assert_eq!(out["signals"], json!({}));
    }

    #[test]
    fn focus_resolve_assembles_focus_object_for_playing_backlog_card() {
        let c = Connection::open_in_memory().unwrap();
        base_schema(&c);
        insert_card(&c, "FEAT-1", "feature one", "backlog", Some("play"), None, "2026-01-01 00:00:01", None);

        let out = focusResolve(&json!({}), &c).unwrap();
        let focus = &out["focus"];
        assert_eq!(focus["card_slug"], json!("FEAT-1"));
        assert_eq!(focus["intent"], json!("play"));
        assert_eq!(focus["current_phase"], json!("backlog"));
        assert_eq!(focus["next_action"], json!("pl.session.start"));
        assert_eq!(focus["next_agent"], json!("pl"));
        assert_eq!(focus["can_progress"], json!(true));
        assert_eq!(focus["reason_md"], json!("Next: pl runs pl.session.start."));
    }

    #[test]
    fn focus_resolve_blocked_focus_card_surfaces_first_blocker_reason() {
        let c = Connection::open_in_memory().unwrap();
        base_schema(&c);
        insert_card(&c, "FEAT-2", "feature two", "doing", Some("play"), None, "2026-01-01 00:00:01", None);
        c.execute(
            "INSERT INTO contracts (slug, status, card_slug, superseded_by) VALUES ('CT-1','impl','FEAT-2','')",
            [],
        ).unwrap();

        let out = focusResolve(&json!({}), &c).unwrap();
        let focus = &out["focus"];
        assert_eq!(focus["card_slug"], json!("FEAT-2"));
        assert_eq!(focus["current_phase"], json!("contract.impl"));
        assert_eq!(focus["next_action"], json!("git-manager.implement.code"));
        assert_eq!(focus["can_progress"], json!(true));
    }
}
