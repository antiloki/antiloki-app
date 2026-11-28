//! epic-has-open-children.rs — twin of skills/system/focus-detectors/epic-has-open-children.mjs.
//! NAMES ARE CONTRACT: the export is `detect` (the JS named default). Unlike blocked-status /
//! stopped-intent this detector READS the db (open children of the EPIC) — db-last helper lane.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn js_string(v:&Value)->String{ match v{Value::Null=>"null".into(),Value::String(s)=>s.clone(),Value::Number(n)=>n.to_string(),Value::Bool(b)=>b.to_string(),Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","),Value::Object(_)=>"[object Object]".into()} } // = JS String(v)
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>js_string(x) } } // = JS `${v}`

/// JS: `detect(card, db)` — fires only for EPIC-kind cards (body_json.kind === 'epic') with
/// ≥1 child whose status is outside done/archived. Children come back newest-first.
pub fn detect(card: &Value, db: &Connection) -> Value {
    // JS: try { body = card.body_json ? JSON.parse(card.body_json) : null } catch { body = null }
    let body: Value = match card.get("body_json") {
        Some(v) if truthy(Some(v)) => serde_json::from_str(&js_string(v)).unwrap_or(Value::Null),
        _ => Value::Null,
    };
    if !truthy(Some(&body)) || body.get("kind").and_then(|v| v.as_str()) != Some("epic") {
        return Value::Null;
    }

    let open_children: Vec<Value> = crate::db_json::query_rows(
        db,
        "\n    SELECT slug, status FROM cards\n    WHERE parent_slug = ? AND status NOT IN ('done', 'archived')\n    ORDER BY updated_at DESC\n  ",
        &[card.get("slug").cloned().unwrap_or(Value::Null)],
    )
    .ok()
    .and_then(|r| r.as_array().cloned())
    .unwrap_or_default();

    let n = open_children.len();
    if n == 0 {
        return Value::Null;
    }

    let mut detail = Map::new();
    detail.insert("open_count".into(), json!(n));
    detail.insert(
        "open_children".into(),
        Value::Array(
            open_children.iter().take(10).map(|c| {
                let mut o = Map::new();
                o.insert("slug".into(), c.get("slug").cloned().unwrap_or(Value::Null));
                o.insert("status".into(), c.get("status").cloned().unwrap_or(Value::Null));
                Value::Object(o)
            }).collect(),
        ),
    );
    detail.insert("truncated".into(), json!(n > 10));

    // JS Array.join renders null/undefined elements as '' (unlike `${v}` → tmpl).
    let first3: Vec<String> = open_children.iter().take(3)
        .map(|c| match c.get("slug") { None | Some(Value::Null) => String::new(), Some(v) => js_string(v) })
        .collect();
    let reason_md = format!(
        "EPIC has {} open child{} ({}{}). An EPIC cannot make progress directly — its progress is its children's progress. Close the children or change driver_intent.",
        n,
        if n == 1 { "" } else { "ren" },
        first3.join(", "),
        if n > 3 { ", …" } else { "" },
    );

    let first_slug = open_children[0].get("slug");
    let mut opt_a = Map::new();
    opt_a.insert("id".into(), json!("A"));
    opt_a.insert("label".into(), json!(format!("Jump to first open child ({})", tmpl(first_slug))));
    opt_a.insert("action".into(), json!("nav_card"));
    opt_a.insert("card_slug".into(), first_slug.cloned().unwrap_or(Value::Null));
    let mut opt_b = Map::new();
    opt_b.insert("id".into(), json!("B"));
    opt_b.insert("label".into(), json!("Mark EPIC paused (operator backs off)"));
    opt_b.insert("action".into(), json!("set_intent"));
    if let Some(slug) = card.get("slug") {
        opt_b.insert("card_slug".into(), slug.clone());
    }
    opt_b.insert("intent".into(), json!("paused"));
    let mut opt_c = Map::new();
    opt_c.insert("id".into(), json!("C"));
    opt_c.insert("label".into(), json!("Show all open children in Fixit"));
    opt_c.insert("action".into(), json!("show_children"));
    if let Some(slug) = card.get("slug") {
        opt_c.insert("card_slug".into(), slug.clone());
    }
    json!({
        "kind": "epic_has_open_children",
        "detail": detail,
        "reason_md": reason_md,
        "surfaces_in": ["board:red-badge", "pipeline:blocked-pane", "fixit:finding"],
        "fix_options": [opt_a, opt_b, opt_c],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(rows: &[(&str, Option<&str>, &str, Option<&str>, &str)]) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE cards (slug TEXT PRIMARY KEY, parent_slug TEXT, status TEXT, body_json TEXT, updated_at TEXT)").unwrap();
        for r in rows {
            c.execute(
                "INSERT INTO cards (slug, parent_slug, status, body_json, updated_at) VALUES (?, ?, ?, ?, ?)",
                rusqlite::params![r.0, r.1, r.2, r.3, r.4],
            ).unwrap();
        }
        c
    }

    #[test]
    fn non_epic_and_bad_body_return_null() {
        let c = db(&[("K1", Some("E1"), "backlog", None, "2026-01-01 00:00:01")]);
        assert_eq!(detect(&json!({"slug":"E1","status":"backlog"}), &c), Value::Null);
        assert_eq!(detect(&json!({"slug":"E1","body_json":null}), &c), Value::Null);
        assert_eq!(detect(&json!({"slug":"E1","body_json":"{not json"}), &c), Value::Null);
        assert_eq!(detect(&json!({"slug":"E1","body_json":"null"}), &c), Value::Null);
        assert_eq!(detect(&json!({"slug":"E1","body_json":"{\"kind\":\"feature\"}"}), &c), Value::Null);
    }

    #[test]
    fn epic_without_open_children_returns_null() {
        let c = db(&[
            ("K1", Some("E1"), "done", None, "2026-01-01 00:00:01"),
            ("K2", Some("E1"), "archived", None, "2026-01-01 00:00:02"),
            ("Z1", Some("E2"), "backlog", None, "2026-01-01 00:00:03"),
        ]);
        assert_eq!(detect(&json!({"slug":"E1","body_json":"{\"kind\":\"epic\"}"}), &c), Value::Null);
    }

    #[test]
    fn singular_child_fires_without_ellipsis() {
        let c = db(&[("K1", Some("E1"), "backlog", None, "2026-01-01 00:00:01")]);
        let f = detect(&json!({"slug":"E1","body_json":"{\"kind\":\"epic\"}"}), &c);
        assert_eq!(f["kind"], json!("epic_has_open_children"));
        assert_eq!(f["detail"]["open_count"], json!(1));
        assert_eq!(f["detail"]["truncated"], json!(false));
        assert_eq!(f["reason_md"].as_str().unwrap().starts_with("EPIC has 1 open child (K1). "), true);
        assert_eq!(f["fix_options"][0]["label"], json!("Jump to first open child (K1)"));
        assert_eq!(f["fix_options"][1]["card_slug"], json!("E1"));
        assert_eq!(f["fix_options"][1]["intent"], json!("paused"));
        assert_eq!(f["fix_options"][2]["action"], json!("show_children"));
    }

    #[test]
    fn plural_newest_first_with_ellipsis() {
        let c = db(&[
            ("K1", Some("E1"), "backlog", None, "2026-01-01 00:00:01"),
            ("K2", Some("E1"), "blocked", None, "2026-01-01 00:00:02"),
            ("K3", Some("E1"), "backlog", None, "2026-01-01 00:00:03"),
            ("K4", Some("E1"), "doing", None, "2026-01-01 00:00:04"),
            ("KD", Some("E1"), "done", None, "2026-01-01 00:00:05"),
        ]);
        let f = detect(&json!({"slug":"E1","body_json":"{\"kind\":\"epic\"}"}), &c);
        assert_eq!(f["detail"]["open_count"], json!(4));
        assert!(f["reason_md"].as_str().unwrap().starts_with("EPIC has 4 open children (K4, K3, K2, …). "));
        assert_eq!(f["fix_options"][0]["card_slug"], json!("K4"));
        assert_eq!(f["detail"]["open_children"][0], json!({"slug":"K4","status":"doing"}));
    }

    #[test]
    fn twelve_children_truncate_to_ten() {
        let rows: Vec<(String, String)> = (1..=12).map(|i| (format!("T{i:02}"), format!("2026-01-01 00:00:{i:02}"))).collect();
        let seed: Vec<(&str, Option<&str>, &str, Option<&str>, &str)> =
            rows.iter().map(|(s, u)| (s.as_str(), Some("E1"), "backlog", None, u.as_str())).collect();
        let c = db(&seed);
        let f = detect(&json!({"slug":"E1","body_json":"{\"kind\":\"epic\"}"}), &c);
        assert_eq!(f["detail"]["open_count"], json!(12));
        assert_eq!(f["detail"]["truncated"], json!(true));
        assert_eq!(f["detail"]["open_children"].as_array().unwrap().len(), 10);
        assert_eq!(f["detail"]["open_children"][0]["slug"], json!("T12"));
        assert!(f["reason_md"].as_str().unwrap().starts_with("EPIC has 12 open children (T12, T11, T10, …). "));
    }
}
