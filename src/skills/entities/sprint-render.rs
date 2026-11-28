//! sprint-render.rs — twin of skills/entities/sprint-render.mjs (db READ lane). NAMES ARE CONTRACT:
//! `sprintRender` (named default). Sprint summary as structured data + markdown.
// QUALITY: 79/100 — JOIN + phase grouping + markdown assembly (filter(Boolean) drops empty lines)
use rusqlite::Connection;
use serde_json::{json, Value};

fn jsnum(v: &Value) -> String {
    // Mirror JS Number→string: integer-valued floats print without a decimal, and -0 collapses to 0
    // (JS `${-0}` === "0"; Rust `format!("{}", -0.0)` === "-0").
    if let Some(i) = v.as_i64() { i.to_string() }
    else if let Some(f) = v.as_f64() {
        if f.is_finite() && f.fract() == 0.0 { (f as i64).to_string() } else { format!("{f}") }
    }
    else { v.to_string() }
}

pub fn sprintRender(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = input.get("slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let slug = match slug { Some(s) => s.to_string(), None => return Err("sprint-render: slug is required".into()) };

    let sprint = crate::db_json::query_rows(db, "SELECT * FROM sprints WHERE slug = ?", &[json!(slug)])?;
    let sprint = match sprint.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("sprint-render: sprint '{slug}' not found")),
    };
    let so = sprint.as_object().unwrap();
    let sget = |k: &str| so.get(k).cloned().unwrap_or(Value::Null);

    let items = crate::db_json::query_rows(db,
        "SELECT si.card_slug, si.phase, si.est_pts, c.title, c.status FROM sprint_items si JOIN cards c ON c.slug = si.card_slug \
         WHERE si.sprint_slug = ? ORDER BY si.phase, si.added_at", &[json!(slug)])?;
    let items = items.as_array().cloned().unwrap_or_default();

    let (mut g_impl, mut g_qa, mut g_done) = (vec![], vec![], vec![]);
    for it in &items {
        match it.get("phase").and_then(|v| v.as_str()) {
            Some("impl") => g_impl.push(it.clone()),
            Some("qa") => g_qa.push(it.clone()),
            Some("done") => g_done.push(it.clone()),
            _ => {}
        }
    }
    let sum = |v: &[Value]| -> f64 { v.iter().map(|i| i.get("est_pts").and_then(|x| x.as_f64()).unwrap_or(0.0)).sum() };
    let total = sum(&items);
    let done = sum(&g_done);

    let line = |i: &Value, strike: bool| {
        let cs = i.get("card_slug").and_then(|v| v.as_str()).unwrap_or("");
        let title = i.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let pts = jsnum(i.get("est_pts").unwrap_or(&Value::Null));
        if strike { format!("- ~~`{cs}` — {title}~~ ({pts}pt)") } else { format!("- `{cs}` — {title} ({pts}pt)") }
    };
    let cap = sget("capacity_pts");
    let cap_str = if cap.as_f64().map(|f| f != 0.0).unwrap_or(false) || cap.as_str().map(|s| !s.is_empty()).unwrap_or(false) { jsnum(&cap) } else { "∞".to_string() };
    let goal = sget("goal_md");
    let started = sget("started_at");
    let ended = sget("ended_at");

    let mut lines: Vec<String> = vec![format!("# Sprint: {slug}")];
    lines.push(if goal.as_str().map(|s| !s.is_empty()).unwrap_or(false) { format!("\n**Goal:** {}\n", goal.as_str().unwrap()) } else { String::new() });
    lines.push(format!("**Capacity:** {cap_str} pts · **In:** {} pts · **Done:** {} pts", jsnum(&json!(total)), jsnum(&json!(done))));
    lines.push(if started.as_str().map(|s| !s.is_empty()).unwrap_or(false) { format!("**Started:** {}", started.as_str().unwrap()) } else { String::new() });
    lines.push(if ended.as_str().map(|s| !s.is_empty()).unwrap_or(false) { format!("**Ended:** {}", ended.as_str().unwrap()) } else { String::new() });
    lines.push(String::new());
    lines.push("## Doing (impl)".into());
    for i in &g_impl { lines.push(line(i, false)); }
    lines.push(String::new());
    lines.push("## In QA".into());
    for i in &g_qa { lines.push(line(i, false)); }
    lines.push(String::new());
    lines.push("## Done".into());
    for i in &g_done { lines.push(line(i, true)); }
    let md = lines.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n");

    Ok(json!({
        "slug": slug, "goal_md": goal, "capacity_pts": cap,
        "started_at": started, "ended_at": ended,
        "item_count": items.len(), "total_pts": total, "done_pts": done,
        "grouped": { "impl": g_impl, "qa": g_qa, "done": g_done }, "md": md
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders_grouped_and_md() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE sprints (slug TEXT, goal_md TEXT, capacity_pts REAL, started_at TEXT, ended_at TEXT); \
          CREATE TABLE cards (slug TEXT, title TEXT, status TEXT); \
          CREATE TABLE sprint_items (sprint_slug TEXT, card_slug TEXT, phase TEXT, est_pts REAL, added_at TEXT); \
          INSERT INTO sprints VALUES ('S','ship',8,null,null); INSERT INTO cards VALUES ('C','Do C','open'); \
          INSERT INTO sprint_items VALUES ('S','C','impl',3,'t1');").unwrap();
        let r = sprintRender(&json!({ "slug": "S" }), &c).unwrap();
        assert_eq!(r["item_count"], json!(1));
        assert_eq!(r["total_pts"].as_f64().unwrap(), 3.0);
        assert!(r["md"].as_str().unwrap().contains("# Sprint: S"));
        assert!(r["md"].as_str().unwrap().contains("- `C` — Do C (3pt)"));
        assert!(sprintRender(&json!({ "slug": "X" }), &c).is_err());
    }
}
