//! mcp-render-memory.rs — twin of skills/external/mcp-render-memory.mjs (db READ lane). NAMES ARE
//! CONTRACT: `mcpRenderMemory` (default). Memory inventory (optional type/since_id filter) as MCP
//! markdown, wrapped by the already-converted to-mcp-markdown twin.
// QUALITY: type icons, tags_json parse-with-catch, 200-char situation slices, superseded excluded
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// TYPE_ICONS: correction ⚠ · preference ★ · decision ◆ · lesson ✎ · reference ↗ · narrative ✒;
// unknown/missing type falls back to '·' (JS `TYPE_ICONS[m.type] ?? '·'`).
fn type_icon(t: Option<&Value>) -> &'static str {
    match t.and_then(Value::as_str) {
        Some("correction") => "\u{26A0}",
        Some("preference") => "\u{2605}",
        Some("decision") => "\u{25C6}",
        Some("lesson") => "\u{270E}",
        Some("reference") => "\u{2197}",
        Some("narrative") => "\u{2712}",
        _ => "\u{00B7}",
    }
}

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v:&Value)->String{ match v{Value::Null=>"null".into(),Value::String(s)=>s.clone(),Value::Number(n)=>n.to_string(),Value::Bool(b)=>b.to_string(),Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","),Value::Object(_)=>"[object Object]".into()} } // = JS String(v)
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`

// JS `String(x).slice(0, 200)` — only called on truthy values.
fn slice200(v: Option<&Value>) -> String {
    js_string(v.unwrap_or(&Value::Null)).chars().take(200).collect()
}

pub fn mcpRenderMemory(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    // `const { type = null, limit = 25, since_id = null } = input ?? {}` — defaults fire on
    // MISSING keys only; a present null stays null (falsy for the filters, bound for LIMIT).
    let type_v: Option<&Value> = obj.and_then(|o| o.get("type"));
    let since_v: Option<&Value> = obj.and_then(|o| o.get("since_id"));
    let limit_v: Value = obj.and_then(|o| o.get("limit")).cloned().unwrap_or(json!(25));

    let mut sql = String::from(
        "SELECT id, type, title, guidance, situation, naive_response,\n                    rationale, tags_json, validated, created_at, created_by\n             FROM memory.memories WHERE COALESCE(superseded_by, 0) = 0",
    );
    let mut params: Vec<Value> = Vec::new();
    if truthy(type_v) {
        sql.push_str(" AND type = ?");
        params.push(type_v.unwrap().clone());
    }
    if truthy(since_v) {
        sql.push_str(" AND id >= ?");
        params.push(since_v.unwrap().clone());
    }
    sql.push_str(" ORDER BY id DESC LIMIT ?");
    params.push(limit_v);

    let rows: Vec<Value> = crate::db_json::query_rows(db, &sql, &params)?
        .as_array().cloned().unwrap_or_default();
    let total: Value = crate::db_json::query_rows(
        db,
        "SELECT COUNT(*) AS n FROM memory.memories WHERE COALESCE(superseded_by, 0) = 0",
        &[],
    )?
    .as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).cloned().unwrap_or(Value::Null);

    let mut lines: Vec<String> = Vec::new();
    lines.push("# memory inventory".into());
    lines.push(String::new());
    let filter_note = if truthy(type_v) {
        format!(" (filtered to type=`{}`)", tmpl(type_v))
    } else {
        String::new()
    };
    lines.push(format!("{} of {} active memories{}", rows.len(), tmpl(Some(&total)), filter_note));
    lines.push(String::new());

    if rows.is_empty() {
        lines.push(if truthy(type_v) {
            format!("_no memories with type=`{}`_", tmpl(type_v))
        } else {
            "_no memories yet \u{2014} substrate has no learned context to inject_".into()
        });
        let mut data = Map::new();
        data.insert("memories".into(), json!([]));
        let mut payload = Map::new();
        payload.insert("markdown".into(), json!(lines.join("\n")));
        payload.insert("data".into(), Value::Object(data));
        return crate::external_to_mcp_markdown::toMcpMarkdown(&Value::Object(payload), &json!({}));
    }

    for m in &rows {
        // `let tags = []; try { tags = JSON.parse(m.tags_json ?? '[]') } catch {}` — parse
        // failure keeps []; a parsed non-array has no .length > 0 so it renders no tags line.
        let tags_parsed: Value = match m.get("tags_json") {
            None | Some(Value::Null) => json!([]),
            Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(json!([])),
            Some(other) => serde_json::from_str::<Value>(&js_string(other)).unwrap_or(json!([])),
        };
        let tags: Vec<Value> = tags_parsed.as_array().cloned().unwrap_or_default();
        let icon = type_icon(m.get("type"));
        lines.push(format!(
            "### {} #{} \u{00B7} `{}` \u{00B7} {}",
            icon, tmpl(m.get("id")), tmpl(m.get("type")), tmpl(m.get("title"))
        ));
        if !tags.is_empty() {
            let rendered: Vec<String> = tags.iter().map(|t| format!("`{}`", tmpl(Some(t)))).collect();
            lines.push(format!("tags: {}", rendered.join(" ")));
        }
        let validated_suffix = if truthy(m.get("validated")) { " \u{00B7} \u{2713} validated" } else { "" };
        lines.push(format!(
            "created {} by `{}`{}",
            tmpl(m.get("created_at")), tmpl(m.get("created_by")), validated_suffix
        ));
        if truthy(m.get("guidance")) {
            lines.push(String::new());
            lines.push(format!("**guidance:** {}", tmpl(m.get("guidance"))));
        }
        if truthy(m.get("situation")) {
            lines.push(String::new());
            lines.push(format!("**situation:** {}", slice200(m.get("situation"))));
            if truthy(m.get("naive_response")) {
                lines.push(format!("**naive response:** {}", slice200(m.get("naive_response"))));
            }
            if truthy(m.get("rationale")) {
                lines.push(format!("**rationale:** {}", slice200(m.get("rationale"))));
            }
        }
        lines.push(String::new());
    }

    let mut data = Map::new();
    data.insert("total".into(), total);
    data.insert("returned".into(), json!(rows.len()));
    data.insert("memories".into(), json!(rows));
    let mut payload = Map::new();
    payload.insert("markdown".into(), json!(lines.join("\n")));
    payload.insert("data".into(), Value::Object(data));
    crate::external_to_mcp_markdown::toMcpMarkdown(&Value::Object(payload), &json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH DATABASE ':memory:' AS memory; \
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, type TEXT, title TEXT, guidance TEXT, situation TEXT, naive_response TEXT, rationale TEXT, tags_json TEXT, validated INTEGER, superseded_by INTEGER, created_at TEXT, created_by TEXT)",
        )
        .unwrap();
        c
    }

    #[allow(clippy::too_many_arguments)]
    fn seed(c: &Connection, id: i64, ty: &str, title: &str, guidance: &str, situation: &str,
            naive: Option<&str>, rationale: Option<&str>, tags: &str, validated: i64,
            superseded: Option<i64>) {
        c.execute(
            "INSERT INTO memory.memories (id, type, title, guidance, situation, naive_response, rationale, tags_json, validated, superseded_by, created_at, created_by) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'2026-01-01 00:00:00','test')",
            rusqlite::params![id, ty, title, guidance, situation, naive, rationale, tags, validated, superseded],
        )
        .unwrap();
    }

    fn md(r: &Value) -> &str { r["content"][0]["text"].as_str().unwrap() }

    #[test]
    fn empty_states() {
        let c = db();
        let r = mcpRenderMemory(&json!({}), &c).unwrap();
        assert_eq!(md(&r), "# memory inventory\n\n0 of 0 active memories\n\n_no memories yet \u{2014} substrate has no learned context to inject_");
        assert_eq!(r["_meta"]["antiloki"]["data"], json!({ "memories": [] }));
        let f = mcpRenderMemory(&json!({ "type": "decision" }), &c).unwrap();
        assert_eq!(md(&f), "# memory inventory\n\n0 of 0 active memories (filtered to type=`decision`)\n\n_no memories with type=`decision`_");
    }

    #[test]
    fn renders_inventory_desc_with_icons_tags_and_slices() {
        let c = db();
        let long = "x".repeat(220);
        seed(&c, 1, "lesson", "old lesson", "", "", None, None, "not-json", 0, None);
        seed(&c, 2, "weird", "unknown type", "keep it", "", None, None, "[]", 0, None);
        seed(&c, 3, "correction", "top one", "do Y not X", &long, Some("naive"), Some("because"), "[\"ai\",\"db\"]", 1, None);
        seed(&c, 4, "decision", "superseded", "gone", "", None, None, "[]", 0, Some(3));
        let r = mcpRenderMemory(&json!({}), &c).unwrap();
        let text = md(&r);
        // id DESC, superseded row excluded → 3 of 3.
        assert!(text.starts_with("# memory inventory\n\n3 of 3 active memories\n\n"));
        assert!(!text.contains("superseded"));
        let expected_situation: String = long.chars().take(200).collect();
        assert!(text.contains(&format!(
            "### \u{26A0} #3 \u{00B7} `correction` \u{00B7} top one\ntags: `ai` `db`\ncreated 2026-01-01 00:00:00 by `test` \u{00B7} \u{2713} validated\n\n**guidance:** do Y not X\n\n**situation:** {expected_situation}\n**naive response:** naive\n**rationale:** because\n"
        )));
        // unknown type → '·' icon; #2's guidance="keep it" IS non-empty → renders a guidance line
        // (empty situation skips the situation/naive/rationale block); #1 has empty guidance AND
        // situation → no detail lines; bad tags_json ("not-json") → parse fails → no tags line.
        assert!(text.contains("### \u{00B7} #2 \u{00B7} `weird` \u{00B7} unknown type\ncreated 2026-01-01 00:00:00 by `test`\n\n**guidance:** keep it\n\n### \u{270E} #1 \u{00B7} `lesson` \u{00B7} old lesson\ncreated 2026-01-01 00:00:00 by `test`\n"));
        let idx3 = text.find("#3").unwrap();
        let idx2 = text.find("#2").unwrap();
        assert!(idx3 < idx2);
        let d = &r["_meta"]["antiloki"]["data"];
        assert_eq!(d["total"], json!(3));
        assert_eq!(d["returned"], json!(3));
        assert_eq!(d["memories"][0]["id"], json!(3));
        assert_eq!(d["memories"][0]["tags_json"], json!("[\"ai\",\"db\"]"));
    }

    #[test]
    fn filters_type_since_id_and_limit() {
        let c = db();
        seed(&c, 1, "lesson", "L1", "g1", "", None, None, "[]", 0, None);
        seed(&c, 2, "correction", "C2", "g2", "", None, None, "[]", 0, None);
        seed(&c, 3, "lesson", "L3", "g3", "", None, None, "[]", 0, None);
        let t = mcpRenderMemory(&json!({ "type": "lesson" }), &c).unwrap();
        assert!(md(&t).contains("2 of 3 active memories (filtered to type=`lesson`)"));
        assert_eq!(t["_meta"]["antiloki"]["data"]["returned"], json!(2));
        let s = mcpRenderMemory(&json!({ "since_id": 2, "limit": 1 }), &c).unwrap();
        let d = &s["_meta"]["antiloki"]["data"];
        assert_eq!(d["returned"], json!(1));
        assert_eq!(d["total"], json!(3));
        assert_eq!(d["memories"][0]["id"], json!(3)); // DESC → highest id first
        assert!(md(&s).contains("1 of 3 active memories\n"));
    }
}
