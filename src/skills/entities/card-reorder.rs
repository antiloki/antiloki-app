//! card-reorder.rs — twin of skills/entities/card-reorder.mjs (db WRITE lane). NAMES ARE CONTRACT:
//! `cardReorder` (default). Assigns cards.display_order from a position spec (to_top / to_bottom /
//! before / after / absolute), bootstrapping NULL-ordered siblings to 10,20,30 when a neighbor has
//! no order. datetime('now') → frozen 2026-01-01 00:00:00 (conform-db frozen-clock lane).
use rusqlite::Connection;
use serde_json::{json, Value};

// display_order as Option<f64>: None ≙ SQL NULL (matches JS `x == null`); Some ≙ a numeric order.
fn dord(row: &Value) -> Option<f64> {
    match row.get("display_order") {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

// JS truthiness for `if (position.to_top)` / `position.before || position.after`.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// JS `??` right-operand guard: null/undefined fall through, everything else wins.
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

// JS: position[k] !== undefined && !== null && !== false  → the key counts as "set".
fn is_set(v: Option<&Value>) -> bool {
    !matches!(v, None | Some(Value::Null) | Some(Value::Bool(false)))
}

// JS source: `datetime('now')` — the JS lane's freezeDb rewrites 'now' to the frozen instant; the
// Rust twin has no hook, so we write the frozen literal directly. Same string in bootstrap + final.
// A fn, not a const: the frozen-vs-wall-clock choice is a RUNTIME decision (crate::clock reads the
// env), which a const initializer cannot make. Same string in bootstrap + final, as before.
fn update_sql() -> String {
    format!("UPDATE cards SET display_order = ?, updated_at = {} WHERE slug = ?", crate::clock::now_sql())
}

fn load_siblings(db: &Connection, parent_slug: &Value, exclude_slug: Option<&Value>) -> Result<Vec<Value>, String> {
    let (sql, params): (&str, Vec<Value>) = match exclude_slug {
        Some(ex) => (
            "SELECT slug, display_order, created_at, id FROM cards \
             WHERE COALESCE(parent_slug, '') = COALESCE(?, '') AND slug != ? \
             ORDER BY COALESCE(display_order, 1e18) ASC, created_at ASC, id ASC",
            vec![parent_slug.clone(), ex.clone()],
        ),
        None => (
            "SELECT slug, display_order, created_at, id FROM cards \
             WHERE COALESCE(parent_slug, '') = COALESCE(?, '') \
             ORDER BY COALESCE(display_order, 1e18) ASC, created_at ASC, id ASC",
            vec![parent_slug.clone()],
        ),
    };
    Ok(crate::db_json::query_rows(db, sql, &params)?
        .as_array()
        .cloned()
        .unwrap_or_default())
}

// Assign 10, 20, 30, ... to siblings in current effective order, ONLY to NULL-order rows. Existing
// orders are preserved; nextOrder is bumped past them so subsequent NULLs don't collide. Returns the
// same list with display_order populated (used by the before/after midpoint math).
fn bootstrap_orders(db: &Connection, siblings: &[Value]) -> Vec<Value> {
    let mut next_order: f64 = 10.0;
    let mut updated: Vec<Value> = Vec::with_capacity(siblings.len());
    for s in siblings {
        match dord(s) {
            None => {
                let slug = s.get("slug").cloned().unwrap_or(Value::Null);
                let _ = crate::db_json::run_write(db, &update_sql(), &[json!(next_order), slug]);
                let mut o = s.as_object().cloned().unwrap_or_default();
                o.insert("display_order".into(), json!(next_order));
                updated.push(Value::Object(o));
                next_order += 10.0;
            }
            Some(d) => {
                if d >= next_order {
                    next_order = (d / 10.0 + 1.0).floor() * 10.0;
                }
                updated.push(s.clone());
            }
        }
    }
    updated
}

pub fn cardReorder(input: &Value, db: &Connection) -> Result<Value, String> {
    let empty = json!({});
    let input = if input.is_null() { &empty } else { input }; // JS: input ?? {}
    let slug = input.get("slug");
    if !truthy(slug) {
        return Err("card-reorder: slug is required".into());
    }
    let position = input.get("position");
    let is_obj = matches!(position, Some(Value::Object(_)) | Some(Value::Array(_)));
    if !truthy(position) || !is_obj {
        return Err("card-reorder: position is required".into());
    }
    let get = |k: &str| position.and_then(|p| p.get(k));

    let position_keys = ["to_top", "to_bottom", "before", "after", "absolute"];
    let set_count = position_keys.iter().filter(|&&k| is_set(get(k))).count();
    if set_count == 0 {
        return Err("card-reorder: position must set exactly one of to_top|to_bottom|before|after|absolute".into());
    }
    if set_count > 1 {
        return Err(format!("card-reorder: position must set exactly one; got {set_count}"));
    }

    let card_rows = crate::db_json::query_rows(
        db,
        "SELECT slug, parent_slug, display_order FROM cards WHERE slug = ?",
        &[slug.cloned().unwrap_or(Value::Null)],
    )?;
    let card = match card_rows.as_array().and_then(|a| a.first()) {
        Some(c) => c.clone(),
        None => return Err("card-reorder: card not found".into()),
    };
    let parent = card.get("parent_slug").cloned().unwrap_or(Value::Null);

    let mut new_order: Option<f64> = None;

    let abs = get("absolute");
    if !matches!(abs, None | Some(Value::Null)) {
        // JS: absolute !== undefined && absolute !== null
        match abs {
            Some(Value::Number(x)) if x.as_f64().map_or(false, |f| f.is_finite()) => {
                new_order = Some(x.as_f64().unwrap());
            }
            _ => return Err("card-reorder: absolute must be a finite number".into()),
        }
    } else {
        // siblings EXCLUDING self (we update self anyway)
        let siblings = load_siblings(db, &parent, slug)?;

        if truthy(get("to_top")) {
            let first = siblings.first();
            let first_null = first.map_or(true, |f| dord(f).is_none());
            if first_null {
                if first.is_some() {
                    // first exists but has NULL order → bootstrap so to_top is meaningful
                    bootstrap_orders(db, &siblings);
                    let reloaded = load_siblings(db, &parent, slug)?;
                    let d0 = reloaded.first().and_then(dord).unwrap_or(10.0); // reloaded[0]?.display_order ?? 10
                    new_order = Some(d0 - 1.0);
                } else {
                    new_order = Some(1.0); // empty scope
                }
            } else {
                new_order = Some(dord(first.unwrap()).unwrap() - 1.0);
            }
        } else if truthy(get("to_bottom")) {
            match siblings.last() {
                None => new_order = Some(1.0),
                Some(last) => {
                    if dord(last).is_none() {
                        bootstrap_orders(db, &siblings);
                        let reloaded = load_siblings(db, &parent, slug)?;
                        let dl = reloaded.last().and_then(dord).unwrap_or(10.0); // reloaded[last]?.display_order ?? 10
                        new_order = Some(dl + 1.0);
                    } else {
                        new_order = Some(dord(last).unwrap() + 1.0);
                    }
                }
            }
        } else if truthy(get("before")) || truthy(get("after")) {
            let target_slug = nz(get("before")).or(nz(get("after"))); // before ?? after
            let target_idx = match siblings.iter().position(|s| s.get("slug") == target_slug) {
                Some(i) => i,
                None => return Err("card-reorder: target sibling not found in scope".into()),
            };
            // If any sibling has NULL order, bootstrap the whole scope first.
            let has_null = siblings.iter().any(|s| dord(s).is_none());
            let working: Vec<Value> = if has_null { bootstrap_orders(db, &siblings) } else { siblings.clone() };
            let target = &working[target_idx];
            if truthy(get("before")) {
                if target_idx == 0 {
                    new_order = Some(dord(target).unwrap_or(0.0) - 1.0); // no prev
                } else {
                    let prev = &working[target_idx - 1];
                    new_order = Some((dord(prev).unwrap_or(0.0) + dord(target).unwrap_or(0.0)) / 2.0);
                }
            } else if target_idx + 1 >= working.len() {
                new_order = Some(dord(target).unwrap_or(0.0) + 1.0); // no next
            } else {
                let next = &working[target_idx + 1];
                new_order = Some((dord(target).unwrap_or(0.0) + dord(next).unwrap_or(0.0)) / 2.0);
            }
        }
    }

    // Sanity-check: no NaN/Infinity through.
    let new_order = match new_order {
        Some(f) if f.is_finite() => f,
        _ => return Err("card-reorder: computed display_order is not finite".into()),
    };

    crate::db_json::run_write(db, &update_sql(), &[json!(new_order), slug.cloned().unwrap_or(Value::Null)])?;

    let out = crate::db_json::query_rows(
        db,
        "SELECT slug, parent_slug, display_order, updated_at FROM cards WHERE slug = ?",
        &[slug.cloned().unwrap_or(Value::Null)],
    )?;
    Ok(out.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mkdb() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT, parent_slug TEXT, display_order, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();
        c
    }
    fn seed(c: &Connection, id: i64, slug: &str, parent: Option<&str>, ord: Option<f64>, created: &str) {
        c.execute(
            "INSERT INTO cards (id, slug, parent_slug, display_order, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?5)",
            rusqlite::params![id, slug, parent, ord, created],
        )
        .unwrap();
    }
    fn ord(c: &Connection, slug: &str) -> Option<f64> {
        crate::db_json::query_rows(c, "SELECT display_order FROM cards WHERE slug = ?", &[json!(slug)])
            .unwrap()[0]["display_order"]
            .as_f64()
    }

    #[test]
    fn reorders() {
        // absolute sets the literal value + stamps the frozen updated_at
        let c = mkdb();
        seed(&c, 1, "X", None, None, "2025-01-01 00:00:00");
        let r = cardReorder(&json!({ "slug": "X", "position": { "absolute": 42.5 } }), &c).unwrap();
        assert_eq!(r["display_order"], json!(42.5));
        assert_eq!(r["updated_at"], json!("2026-01-01 00:00:00"));

        // to_top with an ordered first sibling → first - 1
        let c = mkdb();
        seed(&c, 1, "A", None, Some(5.0), "2025-01-01 00:00:00");
        seed(&c, 2, "B", None, None, "2025-01-02 00:00:00");
        cardReorder(&json!({ "slug": "B", "position": { "to_top": true } }), &c).unwrap();
        assert_eq!(ord(&c, "B"), Some(4.0));

        // to_bottom with an ordered last sibling → last + 1
        let c = mkdb();
        seed(&c, 1, "A", None, Some(5.0), "2025-01-01 00:00:00");
        seed(&c, 2, "B", None, None, "2025-01-02 00:00:00");
        cardReorder(&json!({ "slug": "B", "position": { "to_bottom": true } }), &c).unwrap();
        assert_eq!(ord(&c, "B"), Some(6.0));

        // before midpoint between neighbors
        let c = mkdb();
        seed(&c, 1, "A", None, Some(10.0), "2025-01-01 00:00:00");
        seed(&c, 2, "B", None, Some(20.0), "2025-01-02 00:00:00");
        seed(&c, 3, "C", None, Some(30.0), "2025-01-03 00:00:00");
        cardReorder(&json!({ "slug": "C", "position": { "before": "B" } }), &c).unwrap();
        assert_eq!(ord(&c, "C"), Some(15.0));

        // before/after bootstraps NULL siblings to 10,20,... then places
        let c = mkdb();
        seed(&c, 1, "A", None, None, "2025-01-01 00:00:00");
        seed(&c, 2, "B", None, None, "2025-01-02 00:00:00");
        seed(&c, 3, "C", None, None, "2025-01-03 00:00:00");
        cardReorder(&json!({ "slug": "C", "position": { "before": "B" } }), &c).unwrap();
        assert_eq!(ord(&c, "A"), Some(10.0));
        assert_eq!(ord(&c, "B"), Some(20.0));
        assert_eq!(ord(&c, "C"), Some(15.0));

        // scope is per parent_slug (a non-null parent leaves the root scope untouched)
        let c = mkdb();
        seed(&c, 1, "EPIC-X", None, None, "2025-01-01 00:00:00");
        seed(&c, 2, "CHILD-1", Some("EPIC-X"), Some(100.0), "2025-01-02 00:00:00");
        seed(&c, 3, "CHILD-2", Some("EPIC-X"), Some(200.0), "2025-01-03 00:00:00");
        seed(&c, 4, "FREE-1", None, Some(50.0), "2025-01-04 00:00:00");
        cardReorder(&json!({ "slug": "CHILD-2", "position": { "to_top": true } }), &c).unwrap();
        assert_eq!(ord(&c, "CHILD-2"), Some(99.0));
        assert_eq!(ord(&c, "FREE-1"), Some(50.0));
        assert_eq!(ord(&c, "EPIC-X"), None);

        // missing card → Err (guard path)
        let c = mkdb();
        assert!(cardReorder(&json!({ "slug": "NOPE", "position": { "to_top": true } }), &c).is_err());
    }
}
