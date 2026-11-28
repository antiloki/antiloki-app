//! board-evaluate.rs — twin of skills/system/board-evaluate.mjs (db READ lane). NAMES ARE
//! CONTRACT: `boardEvaluate` (default). Board-wide aggregation + the top_recommendation
//! priority chain (reds > in-flight > doing > free > stale > epics > idle), owned by the
//! substrate, not the UI. datetime('now','-7 days') frozen to 2026-01-01 00:00:00 per the crossing clock.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS `${v}` interpolation (string/number/null appear here).
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

const CARD_COLS: &str = "slug, title, status, body_json, risk, complexity, priority, score,
                   parent_slug, created_at, updated_at,
                   COALESCE(driver_intent, 'auto') AS driver_intent";

fn pick_top_recommendation(open_red_notifications: i64, in_flight: &[Value], counts_doing: i64, free_cards: &[Value], stale_cards: &[Value], epics: &[Value]) -> Value {
    if open_red_notifications > 0 {
        return json!({
            "title": format!("Address {} red notification{}", open_red_notifications, if open_red_notifications == 1 { "" } else { "s" }),
            "hint": "Driver is halted on red. Open Notifications mode (n) \u{2192} dismiss or act.",
            "severity": "warn",
        });
    }
    if !in_flight.is_empty() {
        let ct = &in_flight[0];
        return json!({
            "title": format!("Continue {} (contract {}, {})", tmpl(ct.get("card_slug")), tmpl(ct.get("contract_slug")), tmpl(ct.get("status"))),
            "hint": "Pipeline is moving. Open the card to inspect current phase.",
            "severity": "go",
        });
    }
    if counts_doing > 0 {
        return json!({
            "title": format!("Resume {} card{} in doing", counts_doing, if counts_doing == 1 { "" } else { "s" }),
            "hint": "Driver picks up next tick; check Pipeline mode (p) for current phase.",
            "severity": "go",
        });
    }
    if !free_cards.is_empty() {
        let c = &free_cards[0];
        return json!({
            "title": format!("Drive {} \u{2014} free + oldest in backlog", tmpl(c.get("slug"))),
            "hint": "Triage (t) \u{2192} space to play, or file a contract via PL ratify.",
            "severity": "go",
        });
    }
    if !stale_cards.is_empty() {
        let n = stale_cards.len();
        return json!({
            "title": format!("Triage {} stale backlog card{}", n, if n == 1 { "" } else { "s" }),
            "hint": "Cards untouched > 7d. Triage mode (t) \u{2192} keep/modify/defer/archive each.",
            "severity": "warn",
        });
    }
    if !epics.is_empty() {
        let pct_of = |e: &Value| e.get("progress_pct").and_then(|v| v.as_i64()).unwrap_or(0);
        // JS reduce((a,b) => a.progress_pct > b.progress_pct ? a : b) — ties take the LATER element
        let mut e = &epics[0];
        for b in &epics[1..] {
            if pct_of(e) <= pct_of(b) { e = b; }
        }
        let pct = pct_of(e);
        if pct == 100 {
            return json!({ "title": format!("Close {} \u{2014} all children done", tmpl(e.get("slug"))), "hint": "EPIC ready to mark done.", "severity": "go" });
        }
        return json!({ "title": format!("Push {} ({}% done)", tmpl(e.get("slug")), pct), "hint": "Furthest-along open EPIC \u{2014} best ROI on a final sprint.", "severity": "go" });
    }
    json!({
        "title": "Board is clear",
        "hint": "No in-flight work, no free cards, no stale items. File new work or rest.",
        "severity": "idle",
    })
}

pub fn boardEvaluate(_input: &Value, db: &Connection) -> Result<Value, String> {
    let count = |sql: &str| -> Result<i64, String> {
        let rows = crate::db_json::query_rows(db, sql, &[])?;
        Ok(rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("c")).and_then(|c| c.as_i64()).unwrap_or(0))
    };
    let list = |sql: &str| -> Result<Vec<Value>, String> {
        Ok(crate::db_json::query_rows(db, sql, &[])?.as_array().cloned().unwrap_or_default())
    };

    let mut counts = Map::new();
    counts.insert("backlog".into(), json!(count("SELECT COUNT(*) AS c FROM cards WHERE status='backlog'")?));
    counts.insert("doing".into(), json!(count("SELECT COUNT(*) AS c FROM cards WHERE status='doing'")?));
    counts.insert("done".into(), json!(count("SELECT COUNT(*) AS c FROM cards WHERE status='done'")?));
    counts.insert("archived".into(), json!(count("SELECT COUNT(*) AS c FROM cards WHERE status='archived'")?));
    counts.insert("total".into(), json!(count("SELECT COUNT(*) AS c FROM cards")?));
    let counts_doing = counts.get("doing").and_then(|v| v.as_i64()).unwrap_or(0);

    let epic_rows = list(
        "SELECT
      e.slug, e.title, e.status,
      (SELECT COUNT(*) FROM cards c WHERE c.parent_slug = e.slug)                                AS total_children,
      (SELECT COUNT(*) FROM cards c WHERE c.parent_slug = e.slug AND c.status = 'done')          AS done_children,
      (SELECT COUNT(*) FROM cards c WHERE c.parent_slug = e.slug AND c.status = 'doing')         AS doing_children,
      (SELECT COUNT(*) FROM cards c WHERE c.parent_slug = e.slug AND c.status = 'backlog')       AS backlog_children
    FROM cards e
    WHERE e.slug LIKE 'EPIC-%' AND e.status NOT IN ('done', 'archived')
    ORDER BY total_children DESC
    LIMIT 8",
    )?;
    let epics: Vec<Value> = epic_rows.iter().map(|r| {
        // {...r, progress_pct} — spread keeps column order, progress_pct appended
        let mut o = r.as_object().cloned().unwrap_or_default();
        let total = r.get("total_children").and_then(|v| v.as_i64()).unwrap_or(0);
        let done = r.get("done_children").and_then(|v| v.as_i64()).unwrap_or(0);
        let pct = if total == 0 { 0 } else { ((done as f64 / total as f64) * 100.0).round() as i64 };
        o.insert("progress_pct".into(), json!(pct));
        Value::Object(o)
    }).collect();

    let free_cards = list(&format!(
        "SELECT {CARD_COLS}
    FROM cards c
    WHERE c.status = 'backlog'
      AND c.parent_slug IS NULL
      AND c.slug NOT LIKE 'EPIC-%'
      AND NOT EXISTS (
        SELECT 1 FROM contracts ct
        WHERE ct.card_slug = c.slug AND ct.status IN ('proposed', 'impl')
      )
    ORDER BY c.created_at ASC
    LIMIT 5"
    ))?;

    let stale_cards = list(&format!(
        "SELECT {CARD_COLS}
    FROM cards
    WHERE status = 'backlog'
      AND updated_at < datetime({NOW}, '-7 days')
    ORDER BY updated_at ASC
    LIMIT 5",
        NOW = crate::clock::now_sql_inner()
    ))?;

    let in_flight = list(
        "SELECT ct.slug AS contract_slug, ct.card_slug, c.title AS card_title, ct.status, ct.size
    FROM contracts ct
    LEFT JOIN cards c ON c.slug = ct.card_slug
    WHERE ct.status IN ('proposed', 'impl')
      AND COALESCE(ct.superseded_by, '') = ''
    ORDER BY ct.updated_at DESC
    LIMIT 5",
    )?;

    let recent_done = list(&format!(
        "SELECT {CARD_COLS}
    FROM cards
    WHERE status = 'done'
      AND updated_at >= datetime({NOW}, '-7 days')
    ORDER BY updated_at DESC
    LIMIT 5",
        NOW = crate::clock::now_sql_inner()
    ))?;

    let velocity_7d = count(
        &format!("SELECT COUNT(*) AS c FROM cards
    WHERE status='done' AND updated_at >= datetime({}, '-7 days')", crate::clock::now_sql_inner()),
    )?;

    // notifications table may not exist in minimal test DBs — JS wraps in try/catch → 0
    let open_red_notifications = crate::db_json::query_rows(db, "SELECT COUNT(*) AS c FROM notifications WHERE dismissed_at IS NULL AND severity='red'", &[])
        .ok()
        .and_then(|r| r.as_array().and_then(|a| a.first()).and_then(|x| x.get("c")).and_then(|c| c.as_i64()))
        .unwrap_or(0);

    let top_recommendation = pick_top_recommendation(open_red_notifications, &in_flight, counts_doing, &free_cards, &stale_cards, &epics);

    let mut out = Map::new();
    out.insert("counts".into(), Value::Object(counts));
    out.insert("epics".into(), json!(epics));
    out.insert("free_cards".into(), json!(free_cards));
    out.insert("stale_cards".into(), json!(stale_cards));
    out.insert("in_flight".into(), json!(in_flight));
    out.insert("recent_done".into(), json!(recent_done));
    out.insert("velocity_7d".into(), json!(velocity_7d));
    out.insert("open_red_notifications".into(), json!(open_red_notifications));
    out.insert("top_recommendation".into(), top_recommendation);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, body_json TEXT, risk INTEGER, complexity INTEGER, priority INTEGER, score INTEGER, parent_slug TEXT, driver_intent TEXT, created_at TEXT, updated_at TEXT); \
             CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT, status TEXT, size TEXT, superseded_by TEXT, updated_at TEXT); \
             CREATE TABLE notifications (id INTEGER PRIMARY KEY, severity TEXT, dismissed_at TEXT);",
        ).unwrap();
        c
    }
    fn add_card(c: &Connection, slug: &str, status: &str, parent: Option<&str>, created: &str, updated: &str) {
        c.execute(
            "INSERT INTO cards (slug, title, status, parent_slug, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
            rusqlite::params![slug, slug, status, parent, created, updated],
        ).unwrap();
    }

    #[test]
    fn empty_board_is_idle() {
        let r = boardEvaluate(&json!({}), &make_db()).unwrap();
        assert_eq!(r["counts"], json!({ "backlog": 0, "doing": 0, "done": 0, "archived": 0, "total": 0 }));
        assert_eq!(r["top_recommendation"]["severity"], json!("idle"));
        assert_eq!(r["top_recommendation"]["title"], json!("Board is clear"));
        assert_eq!(r["velocity_7d"], json!(0));
    }

    #[test]
    fn red_notifications_win_over_everything() {
        let c = make_db();
        add_card(&c, "DOING-1", "doing", None, "2025-12-30 00:00:00", "2025-12-30 00:00:00");
        c.execute_batch("INSERT INTO notifications (severity, dismissed_at) VALUES ('red', NULL), ('red', '2025-12-30 00:00:00'), ('yellow', NULL);").unwrap();
        let r = boardEvaluate(&json!({}), &c).unwrap();
        assert_eq!(r["open_red_notifications"], json!(1));
        assert_eq!(r["top_recommendation"]["severity"], json!("warn"));
        assert_eq!(r["top_recommendation"]["title"], json!("Address 1 red notification"));
    }

    #[test]
    fn in_flight_then_doing_then_free() {
        let c = make_db();
        add_card(&c, "CARD-X", "doing", None, "2025-12-30 00:00:00", "2025-12-30 00:00:00");
        c.execute_batch("INSERT INTO contracts (slug, card_slug, status, size, superseded_by, updated_at) VALUES ('CON-A', 'CARD-X', 'impl', 'M', NULL, '2025-12-31 10:00:00');").unwrap();
        let r = boardEvaluate(&json!({}), &c).unwrap();
        assert_eq!(r["top_recommendation"]["title"], json!("Continue CARD-X (contract CON-A, impl)"));
        assert_eq!(r["in_flight"][0]["card_title"], json!("CARD-X"));
        // supersede the contract → doing branch fires
        c.execute_batch("UPDATE contracts SET superseded_by='CON-B' WHERE slug='CON-A';").unwrap();
        let r = boardEvaluate(&json!({}), &c).unwrap();
        assert_eq!(r["top_recommendation"]["title"], json!("Resume 1 card in doing"));
        // no doing → oldest free backlog card wins
        c.execute_batch("UPDATE cards SET status='backlog' WHERE slug='CARD-X';").unwrap();
        add_card(&c, "FREE-OLD", "backlog", None, "2025-12-27 00:00:00", "2025-12-28 00:00:00");
        let r = boardEvaluate(&json!({}), &c).unwrap();
        assert_eq!(r["top_recommendation"]["title"], json!("Drive FREE-OLD \u{2014} free + oldest in backlog"));
    }

    #[test]
    fn stale_and_epic_chain() {
        let c = make_db();
        add_card(&c, "ST-1", "backlog", Some("EPIC-H"), "2025-12-01 00:00:00", "2025-12-10 00:00:00");
        add_card(&c, "ST-2", "backlog", Some("EPIC-H"), "2025-12-01 00:00:00", "2025-12-11 00:00:00");
        let r = boardEvaluate(&json!({}), &c).unwrap();
        assert_eq!(r["top_recommendation"]["title"], json!("Triage 2 stale backlog cards"));
        assert_eq!(r["stale_cards"][0]["slug"], json!("ST-1")); // oldest first
        assert_eq!(r["stale_cards"][0]["driver_intent"], json!("auto")); // COALESCE
    }

    #[test]
    fn epic_reduce_picks_later_on_tie_and_closes_at_100() {
        let c = make_db();
        // EPIC-D 2/3 done → 67; EPIC-A 1/2 → 50; EPIC-B 1/1 → 100 (ordered by total_children DESC)
        add_card(&c, "EPIC-D", "backlog", None, "2025-12-26 00:00:00", "2025-12-30 00:00:00");
        add_card(&c, "D1", "done", Some("EPIC-D"), "2025-12-20 00:00:00", "2025-12-26 00:00:00");
        add_card(&c, "D2", "done", Some("EPIC-D"), "2025-12-20 00:00:00", "2025-12-27 00:00:00");
        add_card(&c, "D3", "backlog", Some("EPIC-D"), "2025-12-20 00:00:00", "2025-12-30 00:00:00");
        add_card(&c, "EPIC-A", "backlog", None, "2025-12-26 00:00:00", "2025-12-30 12:00:00");
        add_card(&c, "A1", "done", Some("EPIC-A"), "2025-12-20 00:00:00", "2025-12-28 00:00:00");
        add_card(&c, "A2", "backlog", Some("EPIC-A"), "2025-12-20 00:00:00", "2025-12-29 12:00:00");
        add_card(&c, "EPIC-B", "backlog", None, "2025-12-26 00:00:00", "2025-12-31 00:00:00");
        add_card(&c, "B1", "done", Some("EPIC-B"), "2025-12-20 00:00:00", "2025-12-29 00:00:00");
        let r = boardEvaluate(&json!({}), &c).unwrap();
        assert_eq!(r["epics"][0]["slug"], json!("EPIC-D"));
        assert_eq!(r["epics"][0]["progress_pct"], json!(67)); // Math.round(2/3*100)
        assert_eq!(r["top_recommendation"]["title"], json!("Close EPIC-B \u{2014} all children done"));
        assert_eq!(r["recent_done"][0]["slug"], json!("B1")); // updated_at DESC
        assert_eq!(r["velocity_7d"], json!(4));
        // drop B1 → EPIC-B has 0 children (pct 0); max is EPIC-D at 67 → Push
        c.execute_batch("DELETE FROM cards WHERE slug='B1';").unwrap();
        let r = boardEvaluate(&json!({}), &c).unwrap();
        assert_eq!(r["top_recommendation"]["title"], json!("Push EPIC-D (67% done)"));
    }

    #[test]
    fn missing_notifications_table_yields_zero() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, body_json TEXT, risk INTEGER, complexity INTEGER, priority INTEGER, score INTEGER, parent_slug TEXT, driver_intent TEXT, created_at TEXT, updated_at TEXT); \
             CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT, status TEXT, size TEXT, superseded_by TEXT, updated_at TEXT);",
        ).unwrap();
        let r = boardEvaluate(&json!({}), &c).unwrap();
        assert_eq!(r["open_red_notifications"], json!(0));
        assert_eq!(r["top_recommendation"]["severity"], json!("idle"));
    }
}
