//! mcp-render-board.rs — twin of skills/external/mcp-render-board.mjs (db READ lane). NAMES ARE
//! CONTRACT: `mcpRenderBoard` (default). Renders the board as MCP markdown + structured counts/cards.
// QUALITY: 4 status lanes (STATUS_ORDER) with icons, per-lane truncation at limit_per_lane, then
// wrapped by the already-converted to-mcp-markdown twin (content[0].text + _meta.antiloki.data).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

const STATUS_ORDER: [&str; 4] = ["backlog", "doing", "done", "archived"];

// STATUS_ICONS: backlog ○ · doing ◐ · done ● · archived · (middle dot).
fn status_icon(s: &str) -> &'static str {
    match s {
        "backlog" => "\u{25CB}",
        "doing" => "\u{25D0}",
        "done" => "\u{25CF}",
        "archived" => "\u{00B7}",
        _ => "",
    }
}

// JS `${v}` interpolation (used for `${c.slug}`).
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

pub fn mcpRenderBoard(input: &Value, db: &Connection) -> Result<Value, String> {
    let limit_per_lane: i64 = input.get("limit_per_lane").and_then(|v| v.as_i64()).unwrap_or(8);

    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, title, status, updated_at FROM cards ORDER BY updated_at DESC",
        &[],
    )?;
    let cards = rows.as_array().cloned().unwrap_or_default();

    // grouped[i] = cards with status === STATUS_ORDER[i], preserving the DESC order.
    let grouped: Vec<Vec<&Value>> = STATUS_ORDER
        .iter()
        .map(|&s| {
            cards
                .iter()
                .filter(|c| c.get("status").and_then(|v| v.as_str()) == Some(s))
                .collect()
        })
        .collect();

    let mut lines: Vec<String> = Vec::new();
    lines.push("# antiloki \u{00B7} board".into());
    lines.push(String::new());
    let summary: Vec<String> = STATUS_ORDER
        .iter()
        .enumerate()
        .map(|(i, &s)| format!("{} {}", grouped[i].len(), s))
        .collect();
    lines.push(format!(
        "**{} cards** total \u{2014} {}",
        cards.len(),
        summary.join(" \u{00B7} ")
    ));
    lines.push(String::new());

    for (i, &status) in STATUS_ORDER.iter().enumerate() {
        let lane = &grouped[i];
        lines.push(format!("## {} {} ({})", status_icon(status), status, lane.len()));
        if lane.is_empty() {
            lines.push("_(empty)_".into());
        } else {
            let take = limit_per_lane.max(0) as usize;
            for c in lane.iter().take(take) {
                let title = match c.get("title") {
                    Some(Value::String(s)) => s.clone(),
                    _ => String::new(),
                };
                let title: String = title.chars().take(80).collect();
                lines.push(format!("- `{}` \u{2014} {}", tmpl(c.get("slug")), title));
            }
            if (lane.len() as i64) > limit_per_lane {
                lines.push(format!(
                    "- _\u{2026} +{} more (raise limit_per_lane to see)_",
                    lane.len() as i64 - limit_per_lane
                ));
            }
        }
        lines.push(String::new());
    }

    // counts: {backlog,doing,done,archived} in STATUS_ORDER (insertion order = output order).
    let mut counts = Map::new();
    for (i, &s) in STATUS_ORDER.iter().enumerate() {
        counts.insert(s.to_string(), json!(grouped[i].len()));
    }
    let cards_out: Vec<Value> = cards
        .iter()
        .map(|c| {
            let mut o = Map::new();
            o.insert("slug".into(), c.get("slug").cloned().unwrap_or(Value::Null));
            o.insert("title".into(), c.get("title").cloned().unwrap_or(Value::Null));
            o.insert("status".into(), c.get("status").cloned().unwrap_or(Value::Null));
            Value::Object(o)
        })
        .collect();

    let mut data = Map::new();
    data.insert("counts".into(), Value::Object(counts));
    data.insert("total".into(), json!(cards.len()));
    data.insert("cards".into(), json!(cards_out));

    let mut payload = Map::new();
    payload.insert("markdown".into(), json!(lines.join("\n")));
    payload.insert("data".into(), Value::Object(data));

    crate::external_to_mcp_markdown::toMcpMarkdown(&Value::Object(payload), &json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with(rows: &[(&str, Option<&str>, Option<&str>, &str)]) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE cards (slug TEXT, title TEXT, status TEXT, updated_at TEXT)")
            .unwrap();
        for (slug, title, status, ua) in rows {
            c.execute(
                "INSERT INTO cards (slug,title,status,updated_at) VALUES (?1,?2,?3,?4)",
                rusqlite::params![slug, title, status, ua],
            )
            .unwrap();
        }
        c
    }

    #[test]
    fn renders_board() {
        let c = db_with(&[
            ("A", Some("Alpha"), Some("backlog"), "2026-01-09"),
            ("B", Some("Beta"), Some("doing"), "2026-01-08"),
            ("C", None, Some("done"), "2026-01-07"),
            ("D", Some("Delta"), Some("archived"), "2026-01-06"),
            ("E", Some("Echo"), None, "2026-01-05"),
            ("F", Some("Foxtrot"), Some("backlog"), "2026-01-04"),
        ]);
        let r = mcpRenderBoard(&json!({}), &c).unwrap();
        let d = &r["_meta"]["antiloki"]["data"];
        assert_eq!(d["counts"], json!({ "backlog": 2, "doing": 1, "done": 1, "archived": 1 }));
        assert_eq!(d["total"], json!(6));
        assert_eq!(d["cards"].as_array().unwrap().len(), 6);
        assert_eq!(d["cards"][2], json!({ "slug": "C", "title": null, "status": "done" }));
        assert_eq!(d["cards"][4], json!({ "slug": "E", "title": "Echo", "status": null }));
        let md = r["content"][0]["text"].as_str().unwrap();
        assert!(md.starts_with(
            "# antiloki \u{00B7} board\n\n**6 cards** total \u{2014} 2 backlog \u{00B7} 1 doing \u{00B7} 1 done \u{00B7} 1 archived\n"
        ));
        assert!(md.contains("## \u{25CB} backlog (2)\n- `A` \u{2014} Alpha\n- `F` \u{2014} Foxtrot\n"));
        // null title renders as empty string after the em dash.
        assert!(md.contains("## \u{25CF} done (1)\n- `C` \u{2014} \n"));
    }

    #[test]
    fn empty_board() {
        let c = db_with(&[]);
        let r = mcpRenderBoard(&json!({}), &c).unwrap();
        let d = &r["_meta"]["antiloki"]["data"];
        assert_eq!(d["total"], json!(0));
        assert_eq!(d["counts"], json!({ "backlog": 0, "doing": 0, "done": 0, "archived": 0 }));
        assert_eq!(d["cards"], json!([]));
        assert_eq!(r["content"][0]["type"], json!("text"));
        let md = r["content"][0]["text"].as_str().unwrap();
        assert_eq!(md.matches("_(empty)_").count(), 4);
    }

    #[test]
    fn truncates_lane() {
        let c = db_with(&[
            ("X1", Some("One"), Some("backlog"), "2026-02-03"),
            ("X2", Some("Two"), Some("backlog"), "2026-02-02"),
            ("X3", Some("Three"), Some("backlog"), "2026-02-01"),
        ]);
        let r = mcpRenderBoard(&json!({ "limit_per_lane": 2 }), &c).unwrap();
        let md = r["content"][0]["text"].as_str().unwrap();
        assert!(md.contains(
            "## \u{25CB} backlog (3)\n- `X1` \u{2014} One\n- `X2` \u{2014} Two\n- _\u{2026} +1 more (raise limit_per_lane to see)_\n"
        ));
        assert_eq!(r["_meta"]["antiloki"]["data"]["total"], json!(3));
    }
}
