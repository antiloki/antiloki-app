//! card-stuck-where.rs — twin of skills/entities/card-stuck-where.mjs (db READ lane).
//! NAMES ARE CONTRACT: `cardStuckWhere` (named default). Latest event per card (ROW_NUMBER),
//! kept if older than `stale` hours and not in phase 'close'. The JS `julianday('now')` is the
//! frozen instant here — the conform-db JS lane rewrites 'now' to the same literal, so both sides
//! run identical SQL against the SAME seeded SQLite and return identical rows.
// QUALITY: 84/100 — faithful clone of param coercion + windowed query; SQL is the whole judgment
use rusqlite::Connection;
use serde_json::{json, Value};

// = JS Number(v)
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}
// node:sqlite binds a whole-valued JS number as INTEGER, a fractional one as REAL — mirror that so
// the bound value is byte-identical to the JS lane (irrelevant to the returned rows, but faithful).
fn num_param(x: f64) -> Value {
    if x.is_finite() && x.fract() == 0.0 { json!(x as i64) } else { json!(x) }
}

pub fn cardStuckWhere(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing when no db — the db lane always injects one, so unreachable here.

    // const { stale_hours = 24, limit = 50 } = input ?? {};  (default only fires when key is absent)
    let stale_hours = input.get("stale_hours").cloned().unwrap_or(json!(24));
    let limit_in = input.get("limit").cloned().unwrap_or(json!(50));

    // const stale = Math.max(0.1, Number(stale_hours) || 24);
    let n = js_number(Some(&stale_hours));
    let n_or = if n == 0.0 || n.is_nan() { 24.0 } else { n };
    let stale = 0.1_f64.max(n_or);

    // Math.min(Math.max(1, Number(limit) || 50), 500)
    let l = js_number(Some(&limit_in));
    let l_or = if l == 0.0 || l.is_nan() { 50.0 } else { l };
    let limit = 1.0_f64.max(l_or).min(500.0);

    // a `let`, not a `const`: crate::clock decides frozen-vs-wall-clock at runtime.
    let sql = format!("\
    WITH latest AS (
      SELECT card_slug, phase, actor, ts,
             ROW_NUMBER() OVER (PARTITION BY card_slug ORDER BY ts DESC, id DESC) AS rn
      FROM card_phase_events
    )
    SELECT l.card_slug, l.phase, l.actor, l.ts,
           ROUND(({now} - julianday(l.ts)) * 24, 2) AS hours_at_phase,
           c.title, c.status
    FROM latest l
    JOIN cards c ON c.slug = l.card_slug
    WHERE l.rn = 1
      AND ({now} - julianday(l.ts)) * 24 >= ?
      AND l.phase NOT IN ('close')
    ORDER BY hours_at_phase DESC
    LIMIT ?", now = crate::clock::julianday_now_sql());

    crate::db_json::query_rows(db, &sql, &[num_param(stale), num_param(limit)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT); \
             CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY, card_slug TEXT, phase TEXT, actor TEXT, ts TEXT); \
             INSERT INTO cards (slug,title,status) VALUES \
               ('OLD','Old card','doing'),('CLOSED','Closed','done'),\
               ('FRESH','Fresh','doing'),('MULTI','Multi','doing'); \
             INSERT INTO card_phase_events (id,card_slug,phase,actor,ts) VALUES \
               (1,'OLD','doing','dani','2025-12-20 00:00:00'),\
               (2,'CLOSED','close','dani','2025-12-01 00:00:00'),\
               (3,'FRESH','doing','dani','2025-12-31 23:00:00'),\
               (4,'MULTI','todo','dani','2025-12-25 00:00:00'),\
               (5,'MULTI','doing','dani','2025-12-27 00:00:00'),\
               (6,'ORPHAN','doing','dani','2025-11-01 00:00:00');",
        ).unwrap();
        c
    }

    #[test]
    fn stuck_cards_ordered_and_filtered() {
        let c = seeded();
        let rows = cardStuckWhere(&json!({}), &c).unwrap();
        let a = rows.as_array().unwrap();
        // included: OLD(288h), MULTI(latest 'doing', 96h). excluded: FRESH(1h<24), CLOSED(phase), ORPHAN(join)
        let slugs: Vec<&str> = a.iter().map(|r| r["card_slug"].as_str().unwrap()).collect();
        assert_eq!(slugs, vec!["OLD", "MULTI"]);
        let multi = a.iter().find(|r| r["card_slug"] == json!("MULTI")).unwrap();
        assert_eq!(multi["phase"], json!("doing")); // latest by ts DESC,id DESC, not 'todo'
        assert_eq!(a[0]["title"], json!("Old card"));
        assert_eq!(a[0]["hours_at_phase"], json!(288.0));
    }

    #[test]
    fn threshold_floor_and_clamps() {
        let c = seeded();
        // stale_hours=200 → only OLD(288). MULTI(96) drops out.
        let r = cardStuckWhere(&json!({ "stale_hours": 200 }), &c).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 1);
        // stale_hours=0.05 → floored to 0.1 → FRESH(1h) now included; order OLD,MULTI,FRESH
        let r = cardStuckWhere(&json!({ "stale_hours": 0.05 }), &c).unwrap();
        let slugs: Vec<&str> = r.as_array().unwrap().iter().map(|x| x["card_slug"].as_str().unwrap()).collect();
        assert_eq!(slugs, vec!["OLD", "MULTI", "FRESH"]);
        // stale_hours=0 → falsy → 24 fallback → OLD,MULTI
        assert_eq!(cardStuckWhere(&json!({ "stale_hours": 0 }), &c).unwrap().as_array().unwrap().len(), 2);
        // limit=1 → top by hours
        assert_eq!(cardStuckWhere(&json!({ "limit": 1 }), &c).unwrap().as_array().unwrap().len(), 1);
        // limit=1000 → clamped to 500 → both matching rows
        assert_eq!(cardStuckWhere(&json!({ "limit": 1000 }), &c).unwrap().as_array().unwrap().len(), 2);
    }

    #[test]
    fn tie_break_higher_id_wins() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT); \
             CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY, card_slug TEXT, phase TEXT, actor TEXT, ts TEXT); \
             INSERT INTO cards (slug,title,status) VALUES ('TIE','Tie','doing'); \
             INSERT INTO card_phase_events (id,card_slug,phase,actor,ts) VALUES \
               (10,'TIE','alpha','dani','2025-12-01 00:00:00'),(11,'TIE','beta','dani','2025-12-01 00:00:00');",
        ).unwrap();
        let r = cardStuckWhere(&json!({}), &c).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 1);
        assert_eq!(r[0]["phase"], json!("beta")); // id 11 > 10 → rn=1
    }
}
