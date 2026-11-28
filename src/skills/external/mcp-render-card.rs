//! mcp-render-card.rs — twin of skills/external/mcp-render-card.mjs (db READ lane). NAMES ARE
//! CONTRACT: `mcpRenderCard` (default). Renders one card (body + phase events + latest active
//! contract + recent timeline) as MCP markdown, wrapped by the already-converted to-mcp-markdown twin.
// QUALITY: body JSON.parse-with-fallback + non-string re-stringify; contract markdown lines are
// gated ATOMICALLY by both inner JSON.parse calls (data.contract itself is unaffected by that catch).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

// JS: `let body = ''; try { body = JSON.parse(card.body_json || '""') } catch { body = card.body_json
// || '' } if (typeof body !== 'string') body = JSON.stringify(body, null, 2)`. A falsy body_json
// (NULL/empty) parses the literal `""`; a parse failure falls back to the raw text; a successfully
// parsed non-string (object/array/number/bool/null) gets pretty-printed at 2-space indent.
fn render_body(body_json: Option<&Value>) -> String {
    let raw = body_json.and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let parse_src = raw.unwrap_or("\"\"");
    match serde_json::from_str::<Value>(parse_src) {
        Ok(Value::String(s)) => s,
        Ok(other) => serde_json::to_string_pretty(&other).unwrap_or_default(),
        Err(_) => raw.unwrap_or("").to_string(),
    }
}

pub fn mcpRenderCard(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = input.get("slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let slug = match slug {
        Some(s) => s.to_string(),
        None => return Err("mcp-render-card: slug is required".into()),
    };

    let card = crate::db_json::query_rows(
        db,
        "SELECT slug, title, status, body_json, risk, complexity, priority, score, parent_slug, created_at, updated_at FROM cards WHERE slug = ?",
        &[json!(slug)],
    )?
    .as_array()
    .and_then(|a| a.first())
    .cloned();
    let Some(card) = card else {
        return Err(format!("mcp-render-card: card '{}' not found", slug));
    };

    let body = render_body(card.get("body_json"));

    let phase_events = crate::db_json::query_rows(
        db,
        "SELECT phase, actor, ts FROM card_phase_events WHERE card_slug = ? ORDER BY id",
        &[json!(slug)],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();

    let contract = crate::db_json::query_rows(
        db,
        "SELECT slug AS contract_slug, status, size, files_touched_json FROM contracts WHERE card_slug = ? AND COALESCE(superseded_by, '') = '' ORDER BY id DESC LIMIT 1",
        &[json!(slug)],
    )?
    .as_array()
    .and_then(|a| a.first())
    .cloned();

    let timeline = crate::db_json::query_rows(
        db,
        "SELECT operation, actor, ts FROM sacred_timeline WHERE entity_type='card' AND entity_id = ? ORDER BY id DESC LIMIT 5",
        &[json!(slug)],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("# `{}` \u{2014} {}", tmpl(card.get("slug")), tmpl(card.get("title"))));
    lines.push(String::new());
    lines.push(format!(
        "**status:** {} \u{00B7} **created:** {} \u{00B7} **updated:** {}",
        tmpl(card.get("status")), tmpl(card.get("created_at")), tmpl(card.get("updated_at"))
    ));
    if truthy(card.get("parent_slug")) {
        lines.push(format!("**parent:** `{}`", tmpl(card.get("parent_slug"))));
    }
    if nz(card.get("score")).is_some() {
        lines.push(format!(
            "**score:** {} (risk={}, complexity={}, priority={})",
            tmpl(card.get("score")), tmpl(card.get("risk")), tmpl(card.get("complexity")), tmpl(card.get("priority"))
        ));
    }
    lines.push(String::new());

    if !body.trim().is_empty() {
        lines.push("## Body".into());
        lines.push(body.clone());
        lines.push(String::new());
    }

    lines.push(format!("## Phase events ({})", phase_events.len()));
    if phase_events.is_empty() {
        lines.push("_no phase events recorded yet_".into());
    } else {
        lines.push("| phase | actor | timestamp |".into());
        lines.push("|---|---|---|".into());
        for e in &phase_events {
            lines.push(format!("| `{}` | {} | {} |", tmpl(e.get("phase")), tmpl(e.get("actor")), tmpl(e.get("ts"))));
        }
    }
    lines.push(String::new());

    if let Some(contract) = &contract {
        // Both JSON.parse calls sit inside the SAME try — either one throwing skips BOTH
        // lines.push calls below (the `## Contract` header never appears), matching the JS catch.
        let ft_raw = contract.get("files_touched_json").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("[]");
        if let Ok(ft) = serde_json::from_str::<Value>(ft_raw) {
            let mt_row = crate::db_json::query_rows(
                db,
                "SELECT microtasks_json FROM contracts WHERE slug = ?",
                &[contract.get("contract_slug").cloned().unwrap_or(Value::Null)],
            )?
            .as_array()
            .and_then(|a| a.first())
            .cloned();
            let mt_raw = mt_row
                .as_ref()
                .and_then(|r| r.get("microtasks_json"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("[]");
            if let Ok(mt_val) = serde_json::from_str::<Value>(mt_raw) {
                // ft.length / microtasks.length: only the array shape is exercised by real rows
                // (files_touched_json/microtasks_json are always JSON arrays in production).
                let ft_len = ft.as_array().map(|a| a.len()).unwrap_or(0);
                let microtask_count = mt_val.as_array().map(|a| a.len()).unwrap_or(0);
                let size = nz(contract.get("size")).cloned().unwrap_or(json!("?"));
                lines.push(format!("## Contract `{}`", tmpl(contract.get("contract_slug"))));
                lines.push(format!(
                    "status `{}` \u{00B7} size `{}` \u{00B7} {} files touched \u{00B7} {} microtasks",
                    tmpl(contract.get("status")), tmpl(Some(&size)), ft_len, microtask_count
                ));
            }
        }
        lines.push(String::new());
    }

    lines.push(format!("## Recent timeline ({})", timeline.len()));
    if timeline.is_empty() {
        lines.push("_no audited events for this card_".into());
    } else {
        lines.push("| operation | actor | timestamp |".into());
        lines.push("|---|---|---|".into());
        for t in &timeline {
            lines.push(format!("| `{}` | {} | {} |", tmpl(t.get("operation")), tmpl(t.get("actor")), tmpl(t.get("ts"))));
        }
    }

    let mut data = Map::new();
    data.insert("card".into(), card.clone());
    data.insert("phaseEvents".into(), json!(phase_events));
    // contract missing (.get() miss) → JS leaves the property `undefined` → JSON.stringify OMITS
    // the key entirely (not even `null`); insert only when a row was found.
    if let Some(c) = &contract {
        data.insert("contract".into(), c.clone());
    }
    data.insert("timeline".into(), json!(timeline));

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
            "CREATE TABLE cards (slug TEXT, title TEXT, status TEXT, body_json TEXT, risk INTEGER, complexity INTEGER, priority INTEGER, score INTEGER, parent_slug TEXT, created_at TEXT, updated_at TEXT); \
             CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY, card_slug TEXT, phase TEXT, actor TEXT, ts TEXT); \
             CREATE TABLE contracts (id INTEGER PRIMARY KEY, slug TEXT, card_slug TEXT, status TEXT, size TEXT, files_touched_json TEXT, microtasks_json TEXT, superseded_by TEXT); \
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, entity_type TEXT, entity_id TEXT, operation TEXT, actor TEXT, ts TEXT)",
        )
        .unwrap();
        c
    }

    fn md(r: &Value) -> &str { r["content"][0]["text"].as_str().unwrap() }

    #[test]
    fn full_render_with_history_parent_score_and_limits() {
        let c = db();
        c.execute_batch(
            "INSERT INTO cards VALUES ('CARD-1','Ship the widget','doing','\"Plain string body.\"',3,2,4,24,'EPIC-1','2025-12-01 08:00:00','2025-12-15 09:30:00'); \
             INSERT INTO card_phase_events VALUES (1,'CARD-1','research','pl','2025-12-01 08:05:00'),(2,'CARD-1','contract','tl','2025-12-02 09:00:00'),(3,'OTHER','impl','dev','2025-12-03 09:00:00'); \
             INSERT INTO contracts VALUES (1,'K0','CARD-1','rejected','S','[\"x.mjs\"]','[{\"id\":1}]','K1'),(2,'K1','CARD-1','impl','M','[\"a.mjs\",\"b.mjs\"]','[{\"id\":1},{\"id\":2},{\"id\":3}]',NULL); \
             INSERT INTO sacred_timeline VALUES (1,'card','CARD-1','card.add','pl','2025-12-01 08:00:01'),(2,'card','CARD-1','card.status.change','dev','2025-12-01 09:00:01'),(3,'card','OTHER','card.add','pl','2025-12-01 09:30:00'),(4,'contract','K1','contract.add','tl','2025-12-02 09:00:01'),(5,'card','CARD-1','card.update','man','2025-12-03 10:00:00'),(6,'card','CARD-1','card.status.change','dev','2025-12-05 11:00:00'),(7,'card','CARD-1','card.status.change','dev','2025-12-10 12:00:00'),(8,'card','CARD-1','card.review','qa','2025-12-12 13:00:00')",
        )
        .unwrap();
        let r = mcpRenderCard(&json!({ "slug": "CARD-1" }), &c).unwrap();
        let text = md(&r);
        assert!(text.starts_with("# `CARD-1` \u{2014} Ship the widget\n\n**status:** doing \u{00B7} **created:** 2025-12-01 08:00:00 \u{00B7} **updated:** 2025-12-15 09:30:00\n**parent:** `EPIC-1`\n**score:** 24 (risk=3, complexity=2, priority=4)\n\n## Body\nPlain string body.\n\n"));
        assert!(text.contains("## Phase events (2)\n| phase | actor | timestamp |\n|---|---|---|\n| `research` | pl | 2025-12-01 08:05:00 |\n| `contract` | tl | 2025-12-02 09:00:00 |\n"));
        // K0 is superseded_by K1 → excluded; K1 wins by ORDER BY id DESC.
        assert!(text.contains("## Contract `K1`\nstatus `impl` \u{00B7} size `M` \u{00B7} 2 files touched \u{00B7} 3 microtasks\n"));
        // timeline: id DESC LIMIT 5, filtered to entity_type='card' AND entity_id='CARD-1' → 8,7,6,5,2 (id 1 dropped).
        assert!(text.contains("## Recent timeline (5)\n| operation | actor | timestamp |\n|---|---|---|\n| `card.review` | qa | 2025-12-12 13:00:00 |\n| `card.status.change` | dev | 2025-12-10 12:00:00 |\n| `card.status.change` | dev | 2025-12-05 11:00:00 |\n| `card.update` | man | 2025-12-03 10:00:00 |\n| `card.status.change` | dev | 2025-12-01 09:00:01 |"));
        let d = &r["_meta"]["antiloki"]["data"];
        assert_eq!(d["contract"]["contract_slug"], json!("K1"));
        assert_eq!(d["phaseEvents"].as_array().unwrap().len(), 2);
        assert_eq!(d["timeline"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn minimal_card_empty_states_and_omitted_contract_key() {
        let c = db();
        c.execute_batch(
            "INSERT INTO cards VALUES ('CARD-2','Bare card','backlog',NULL,NULL,NULL,NULL,NULL,NULL,'2025-12-20 00:00:00','2025-12-20 00:00:00')",
        )
        .unwrap();
        let r = mcpRenderCard(&json!({ "slug": "CARD-2" }), &c).unwrap();
        let text = md(&r);
        assert!(text.starts_with("# `CARD-2` \u{2014} Bare card\n\n**status:** backlog \u{00B7} **created:** 2025-12-20 00:00:00 \u{00B7} **updated:** 2025-12-20 00:00:00\n\n"));
        assert!(!text.contains("**parent:**"));
        assert!(!text.contains("**score:**"));
        assert!(!text.contains("## Body"));
        assert!(text.contains("## Phase events (0)\n_no phase events recorded yet_\n"));
        assert!(!text.contains("## Contract"));
        assert!(text.contains("## Recent timeline (0)\n_no audited events for this card_"));
        let d = &r["_meta"]["antiloki"]["data"];
        assert!(d.get("contract").is_none(), "contract key must be OMITTED, not null");
        assert_eq!(d["phaseEvents"], json!([]));
        assert_eq!(d["timeline"], json!([]));
    }

    #[test]
    fn score_zero_renders_and_null_contract_size_falls_back_to_question_mark() {
        let c = db();
        c.execute_batch(
            "INSERT INTO cards VALUES ('CARD-3','Score zero','doing','\"b\"',1,1,1,0,'EPIC-9','2025-12-21 00:00:00','2025-12-21 00:00:00'); \
             INSERT INTO contracts VALUES (1,'KC3','CARD-3','proposed',NULL,'[]','[]',NULL)",
        )
        .unwrap();
        let r = mcpRenderCard(&json!({ "slug": "CARD-3" }), &c).unwrap();
        let text = md(&r);
        assert!(text.contains("**score:** 0 (risk=1, complexity=1, priority=1)\n")); // score != null (0 != null) keeps the line
        assert!(text.contains("## Contract `KC3`\nstatus `proposed` \u{00B7} size `?` \u{00B7} 0 files touched \u{00B7} 0 microtasks\n"));
    }

    #[test]
    fn body_object_pretty_prints_and_invalid_json_falls_back_to_raw_text() {
        let c = db();
        c.execute_batch(
            "INSERT INTO cards VALUES ('CARD-4','Object body','doing','{\"summary\":\"x\",\"steps\":[\"a\",\"b\"]}',NULL,NULL,NULL,NULL,NULL,'2025-12-22 00:00:00','2025-12-22 00:00:00'); \
             INSERT INTO cards VALUES ('CARD-5','Bad json body','doing','not valid json {',NULL,NULL,NULL,NULL,NULL,'2025-12-23 00:00:00','2025-12-23 00:00:00'); \
             INSERT INTO cards VALUES ('CARD-6','Whitespace body','doing','\"   \"',NULL,NULL,NULL,NULL,NULL,'2025-12-24 00:00:00','2025-12-24 00:00:00'); \
             INSERT INTO cards VALUES ('CARD-7','Null body','doing','null',NULL,NULL,NULL,NULL,NULL,'2025-12-25 00:00:00','2025-12-25 00:00:00')",
        )
        .unwrap();
        let r4 = mcpRenderCard(&json!({ "slug": "CARD-4" }), &c).unwrap();
        assert!(md(&r4).contains("## Body\n{\n  \"summary\": \"x\",\n  \"steps\": [\n    \"a\",\n    \"b\"\n  ]\n}\n\n"));
        let r5 = mcpRenderCard(&json!({ "slug": "CARD-5" }), &c).unwrap();
        assert!(md(&r5).contains("## Body\nnot valid json {\n\n"));
        let r6 = mcpRenderCard(&json!({ "slug": "CARD-6" }), &c).unwrap();
        assert!(!md(&r6).contains("## Body")); // truthy string but trim() is empty → section skipped
        let r7 = mcpRenderCard(&json!({ "slug": "CARD-7" }), &c).unwrap();
        assert!(md(&r7).contains("## Body\nnull\n\n")); // JSON.parse('null') → null → re-stringified as the text "null"
    }

    #[test]
    fn broken_contract_json_swallows_markdown_but_keeps_raw_row_in_data() {
        let c = db();
        c.execute_batch(
            "INSERT INTO cards VALUES ('CARD-8','Broken ft','doing','\"b\"',NULL,NULL,NULL,NULL,NULL,'2025-12-26 00:00:00','2025-12-26 00:00:00'); \
             INSERT INTO contracts VALUES (1,'KC8','CARD-8','impl','L','not json','[]',NULL); \
             INSERT INTO cards VALUES ('CARD-9','Broken mt','doing','\"b\"',NULL,NULL,NULL,NULL,NULL,'2025-12-27 00:00:00','2025-12-27 00:00:00'); \
             INSERT INTO contracts VALUES (2,'KC9','CARD-9','impl','S','[\"a.mjs\"]','not json',NULL)",
        )
        .unwrap();
        let r8 = mcpRenderCard(&json!({ "slug": "CARD-8" }), &c).unwrap();
        let t8 = md(&r8);
        assert!(!t8.contains("## Contract"));
        assert_eq!(r8["_meta"]["antiloki"]["data"]["contract"]["contract_slug"], json!("KC8"));
        let r9 = mcpRenderCard(&json!({ "slug": "CARD-9" }), &c).unwrap();
        let t9 = md(&r9);
        assert!(!t9.contains("## Contract"));
        assert_eq!(r9["_meta"]["antiloki"]["data"]["contract"]["contract_slug"], json!("KC9"));
    }

    #[test]
    fn missing_slug_and_unknown_card_throw() {
        let c = db();
        assert!(mcpRenderCard(&json!({}), &c).unwrap_err().contains("slug is required"));
        assert!(mcpRenderCard(&json!({ "slug": "GHOST" }), &c).unwrap_err().contains("card 'GHOST' not found"));
    }
}
