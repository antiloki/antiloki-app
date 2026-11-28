//! mcp-render-reasoning.rs — twin of skills/external/mcp-render-reasoning.mjs (db READ lane,
//! brain-attached). NAMES ARE CONTRACT: `mcpRenderReasoning` (default). Renders the "reasoning"
//! panel for one interviewer turn (AI calls + cost/token totals, runtime events, audited
//! mutations) as MCP markdown, wrapped via the already-converted `toMcpMarkdown` twin.
// QUALITY: turn resolution (explicit id vs most-recent, both .get()-style single-row lookups),
// cross-db ai_calls/prompt_templates join filtered by runtime_events.user_goal_id, raw (non-
// toFixed) totals kept int-or-float per js_num, markdown toFixed(6)/toFixed(0) text always keeps
// its decimals (unlike the JSON totals).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
// JS `(x).toFixed(d)` rendered as TEXT (always keeps its `d` decimals — unlike JSON numbers).
fn to_fixed(x: f64, d: usize) -> String { format!("{:.*}", d, x) }
// JS raw-number JSON output (no toFixed applied): whole values render as integers,
// fractionals stay floats (JSON.stringify(5.0) is "5", JSON.stringify(5.5) is "5.5").
fn js_num(n: f64) -> Value { if n.is_finite() && n.fract() == 0.0 && n.abs() < 9e15 { json!(n as i64) } else { json!(n) } }

pub fn mcpRenderReasoning(input: &Value, db: &Connection) -> Result<Value, String> {
    let turn_id = input.get("turn_id");

    let turn: Value = if truthy(turn_id) {
        let tid = turn_id.cloned().unwrap_or(Value::Null);
        let row = crate::db_json::query_rows(db, "SELECT * FROM interviewer_turns WHERE id = ?", &[tid.clone()])?
            .as_array().and_then(|a| a.first()).cloned();
        match row {
            Some(r) => r,
            None => return Err(format!("mcp-render-reasoning: interviewer_turn id={} not found", tmpl(Some(&tid)))),
        }
    } else {
        let row = crate::db_json::query_rows(db, "SELECT * FROM interviewer_turns ORDER BY id DESC LIMIT 1", &[])?
            .as_array().and_then(|a| a.first()).cloned();
        match row {
            Some(r) => r,
            None => {
                let payload = json!({
                    "markdown": "# reasoning\n\n_(no interviewer turns recorded yet)_",
                    "data": { "turn": null },
                });
                return crate::external_to_mcp_markdown::toMcpMarkdown(&payload, &json!({}));
            }
        }
    };

    let tid_val = turn.get("id").cloned().unwrap_or(Value::Null);

    let ai_calls = crate::db_json::query_rows(
        db,
        "SELECT ai.id, ai.provider, ai.model_id, ai.tier, ai.temperature, ai.tokens_in, \
                ai.tokens_out, ai.cost_usd_actual, ai.duration_ms, ai.cached, ai.outcome, \
                pt.name AS prompt_name \
         FROM brain.ai_calls ai \
         LEFT JOIN brain.prompt_templates pt ON pt.id = ai.prompt_template_id \
         WHERE ai.parent_event_id IN (SELECT id FROM runtime_events WHERE user_goal_id = ?) \
            OR EXISTS (SELECT 1 FROM runtime_events re WHERE re.user_goal_id = ? AND re.id = ai.parent_event_id) \
         ORDER BY ai.id",
        &[tid_val.clone(), tid_val.clone()],
    )?.as_array().cloned().unwrap_or_default();

    let rt_events = crate::db_json::query_rows(
        db,
        "SELECT id, action, skill, outcome, duration_ms, source FROM runtime_events WHERE user_goal_id = ? ORDER BY id",
        &[tid_val.clone()],
    )?.as_array().cloned().unwrap_or_default();

    let timeline = crate::db_json::query_rows(
        db,
        "SELECT operation, entity_type, entity_id, ts FROM sacred_timeline WHERE user_goal_id = ? ORDER BY id",
        &[tid_val.clone()],
    )?.as_array().cloned().unwrap_or_default();

    let total_cost: f64 = ai_calls.iter().map(|c| nz(c.get("cost_usd_actual")).and_then(Value::as_f64).unwrap_or(0.0)).sum();
    let total_tokens_in: i64 = ai_calls.iter().map(|c| nz(c.get("tokens_in")).and_then(Value::as_i64).unwrap_or(0)).sum();
    let total_tokens_out: i64 = ai_calls.iter().map(|c| nz(c.get("tokens_out")).and_then(Value::as_i64).unwrap_or(0)).sum();
    let cache_hits = ai_calls.iter().filter(|c| truthy(c.get("cached"))).count();

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("# reasoning \u{00b7} turn #{}", tmpl(turn.get("id"))));
    lines.push(String::new());
    let user_said: String = str_qq(turn.get("user_text")).chars().take(200).collect();
    lines.push(format!("**user said:** {}", user_said));
    let confidence = match nz(turn.get("classification_confidence")) { Some(v) => tmpl(Some(v)), None => "\u{2014}".into() };
    lines.push(format!("**classified as:** {} (confidence {})", tmpl(turn.get("classified_kind")), confidence));
    lines.push(format!(
        "**confirmed:** {} \u{00b7} **session:** {}",
        if truthy(turn.get("confirmed")) { "yes" } else { "no" },
        tmpl(turn.get("session_id"))
    ));
    lines.push(String::new());

    lines.push(format!("## AI calls ({})", ai_calls.len()));
    if ai_calls.is_empty() {
        lines.push("_no AI calls attributed to this turn_".into());
    } else {
        lines.push(format!(
            "**total cost:** ${} \u{00b7} **tokens in/out:** {}/{} \u{00b7} **cache hits:** {}/{}",
            to_fixed(total_cost, 6), total_tokens_in, total_tokens_out, cache_hits, ai_calls.len()
        ));
        lines.push(String::new());
        lines.push("| # | prompt | provider | model | tier | tokens | cost | latency | cached |".into());
        lines.push("|---|---|---|---|---|---|---|---|---|".into());
        for c in &ai_calls {
            let prompt_name = match nz(c.get("prompt_name")) { Some(v) => tmpl(Some(v)), None => "?".into() };
            let tokens_in = match nz(c.get("tokens_in")) { Some(v) => tmpl(Some(v)), None => "0".into() };
            let tokens_out = match nz(c.get("tokens_out")) { Some(v) => tmpl(Some(v)), None => "0".into() };
            let cost = nz(c.get("cost_usd_actual")).and_then(Value::as_f64).unwrap_or(0.0);
            let duration = nz(c.get("duration_ms")).and_then(Value::as_f64).unwrap_or(0.0);
            let cached_mark = if truthy(c.get("cached")) { "\u{2713}" } else { "" };
            lines.push(format!(
                "| {} | `{}` | {} | `{}` | {} | {}\u{2193}/{}\u{2191} | ${} | {}ms | {} |",
                tmpl(c.get("id")), prompt_name, tmpl(c.get("provider")), tmpl(c.get("model_id")), tmpl(c.get("tier")),
                tokens_in, tokens_out, to_fixed(cost, 6), to_fixed(duration, 0), cached_mark
            ));
        }
    }
    lines.push(String::new());

    lines.push(format!("## Runtime events ({})", rt_events.len()));
    if rt_events.is_empty() {
        lines.push("_no runtime events attributed to this turn_".into());
    } else {
        lines.push("| action | skill | outcome | source | latency |".into());
        lines.push("|---|---|---|---|---|".into());
        for r in &rt_events {
            let action = if truthy(r.get("action")) { format!("`{}`", tmpl(r.get("action"))) } else { "\u{2014}".into() };
            let skill = if truthy(r.get("skill")) { format!("`{}`", tmpl(r.get("skill"))) } else { "\u{2014}".into() };
            let source = match nz(r.get("source")) { Some(v) => tmpl(Some(v)), None => "\u{2014}".into() };
            let duration = nz(r.get("duration_ms")).and_then(Value::as_f64).unwrap_or(0.0);
            lines.push(format!("| {} | {} | {} | {} | {}ms |", action, skill, tmpl(r.get("outcome")), source, to_fixed(duration, 0)));
        }
    }
    lines.push(String::new());

    lines.push(format!("## Audited mutations ({})", timeline.len()));
    if timeline.is_empty() {
        lines.push("_no audited mutations attributed to this turn (read-only or denied)_".into());
    } else {
        lines.push("| operation | entity | timestamp |".into());
        lines.push("|---|---|---|".into());
        for t in &timeline {
            lines.push(format!(
                "| `{}` | {}:`{}` | {} |",
                tmpl(t.get("operation")), tmpl(t.get("entity_type")), tmpl(t.get("entity_id")), tmpl(t.get("ts"))
            ));
        }
    }

    let mut totals = Map::new();
    totals.insert("cost_usd".into(), js_num(total_cost));
    totals.insert("tokens_in".into(), json!(total_tokens_in));
    totals.insert("tokens_out".into(), json!(total_tokens_out));
    totals.insert("cache_hits".into(), json!(cache_hits as i64));

    let mut data = Map::new();
    data.insert("turn".into(), turn.clone());
    data.insert("aiCalls".into(), json!(ai_calls));
    data.insert("rtEvents".into(), json!(rt_events));
    data.insert("timeline".into(), json!(timeline));
    data.insert("totals".into(), Value::Object(totals));

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
            "ATTACH ':memory:' AS brain; \
             CREATE TABLE interviewer_turns (id INTEGER PRIMARY KEY, session_id INTEGER, turn_index INTEGER, created_at TEXT, user_text TEXT, classified_kind TEXT, classification_confidence REAL, classified_reason TEXT, proposed_actions_json TEXT, confirmed INTEGER, confirmation_text TEXT, dispatched_action_ids_json TEXT); \
             CREATE TABLE runtime_events (id INTEGER PRIMARY KEY, ts TEXT, parent_event_id INTEGER, actor TEXT, agent TEXT, action TEXT, skill TEXT, duration_ms REAL, outcome TEXT, error_code TEXT, context_json TEXT, user_goal_id INTEGER, source TEXT); \
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT, user_goal_id INTEGER); \
             CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY, name TEXT); \
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, parent_event_id INTEGER, provider TEXT, model_id TEXT, tier TEXT, temperature TEXT, tokens_in INTEGER, tokens_out INTEGER, cost_usd_actual REAL, duration_ms REAL, cached INTEGER, outcome TEXT, prompt_template_id INTEGER);",
        )
        .unwrap();
        c
    }

    fn md(r: &Value) -> &str { r["content"][0]["text"].as_str().unwrap() }

    #[test]
    fn explicit_turn_id_full_render_with_join_filter_and_null_fallbacks() {
        let c = db();
        c.execute_batch(
            "INSERT INTO interviewer_turns (id,session_id,user_text,classified_kind,classification_confidence,confirmed) VALUES (7,42,'hello world','mutate',0.87,1); \
             INSERT INTO runtime_events (id,user_goal_id,action,skill,outcome,duration_ms,source) VALUES \
               (100,7,'card.add','card-add','success',123.4,'mcp'), \
               (101,7,NULL,NULL,'blocked',NULL,NULL); \
             INSERT INTO brain.prompt_templates (id,name) VALUES (1,'dev.impl.microtask'); \
             INSERT INTO brain.ai_calls (id,parent_event_id,provider,model_id,tier,temperature,tokens_in,tokens_out,cost_usd_actual,duration_ms,cached,outcome,prompt_template_id) VALUES \
               (1,100,'anthropic','claude-x','mid','warm',120,340,0.001234,456.7,0,'success',1), \
               (2,100,'anthropic','claude-y','high','cold',NULL,NULL,NULL,NULL,1,'success',NULL), \
               (3,999,'anthropic','claude-z','low','cold',5,5,9.0,10.0,0,'success',1); \
             INSERT INTO sacred_timeline (id,user_goal_id,operation,entity_type,entity_id,ts) VALUES \
               (1,7,'card.update','card','CARD-1','2026-01-01T00:00:01Z'), \
               (2,999,'card.add','card','OTHER','2026-01-01T00:00:02Z')",
        )
        .unwrap();
        let r = mcpRenderReasoning(&json!({ "turn_id": 7 }), &c).unwrap();
        let text = md(&r);
        assert!(text.starts_with("# reasoning \u{00b7} turn #7\n\n**user said:** hello world\n**classified as:** mutate (confidence 0.87)\n**confirmed:** yes \u{00b7} **session:** 42\n\n"));
        // id=3 (parent_event_id=999) is NOT attributed to turn 7 → excluded → 2 calls only.
        assert!(text.contains("## AI calls (2)\n**total cost:** $0.001234 \u{00b7} **tokens in/out:** 120/340 \u{00b7} **cache hits:** 1/2\n"));
        assert!(text.contains("| 1 | `dev.impl.microtask` | anthropic | `claude-x` | mid | 120\u{2193}/340\u{2191} | $0.001234 | 457ms |  |"));
        assert!(text.contains("| 2 | `?` | anthropic | `claude-y` | high | 0\u{2193}/0\u{2191} | $0.000000 | 0ms | \u{2713} |"));
        assert!(text.contains("## Runtime events (2)\n| action | skill | outcome | source | latency |\n|---|---|---|---|---|\n| `card.add` | `card-add` | success | mcp | 123ms |\n| \u{2014} | \u{2014} | blocked | \u{2014} | 0ms |"));
        assert!(text.contains("## Audited mutations (1)\n| operation | entity | timestamp |\n|---|---|---|\n| `card.update` | card:`CARD-1` | 2026-01-01T00:00:01Z |"));
        let d = &r["_meta"]["antiloki"]["data"];
        assert_eq!(d["turn"]["id"], json!(7));
        assert_eq!(d["aiCalls"].as_array().unwrap().len(), 2);
        assert_eq!(d["totals"], json!({ "cost_usd": 0.001234, "tokens_in": 120, "tokens_out": 340, "cache_hits": 1 }));
    }

    #[test]
    fn falsy_turn_id_and_no_turn_id_both_fall_back_to_most_recent_with_empty_sections() {
        let c = db();
        c.execute_batch(
            "INSERT INTO interviewer_turns (id,session_id,user_text,classified_kind,classification_confidence,confirmed) VALUES \
               (1,10,'first','read',NULL,0), \
               (9,10,NULL,'clarify',NULL,0)",
        )
        .unwrap();
        // No turn_id at all → most recent (id 9, ORDER BY id DESC LIMIT 1).
        let r1 = mcpRenderReasoning(&json!({}), &c).unwrap();
        let t1 = md(&r1);
        assert!(t1.starts_with("# reasoning \u{00b7} turn #9\n\n**user said:** \n**classified as:** clarify (confidence \u{2014})\n**confirmed:** no \u{00b7} **session:** 10\n\n"));
        assert!(t1.contains("## AI calls (0)\n_no AI calls attributed to this turn_"));
        assert!(t1.contains("## Runtime events (0)\n_no runtime events attributed to this turn_"));
        assert!(t1.contains("## Audited mutations (0)\n_no audited mutations attributed to this turn (read-only or denied)_"));
        let totals1 = &r1["_meta"]["antiloki"]["data"]["totals"];
        assert_eq!(totals1, &json!({ "cost_usd": 0, "tokens_in": 0, "tokens_out": 0, "cache_hits": 0 }));
        // turn_id: 0 is JS-falsy → same most-recent branch, NOT a lookup for id=0.
        let r2 = mcpRenderReasoning(&json!({ "turn_id": 0 }), &c).unwrap();
        assert_eq!(md(&r2), t1);
    }

    #[test]
    fn no_turns_recorded_yet_short_circuits() {
        let c = db();
        let r = mcpRenderReasoning(&json!({}), &c).unwrap();
        assert_eq!(md(&r), "# reasoning\n\n_(no interviewer turns recorded yet)_");
        assert_eq!(r["_meta"]["antiloki"]["data"], json!({ "turn": null }));
    }

    #[test]
    fn unknown_explicit_turn_id_throws() {
        let c = db();
        c.execute_batch("INSERT INTO interviewer_turns (id,session_id,user_text,classified_kind,confirmed) VALUES (1,10,'x','read',0)").unwrap();
        let err = mcpRenderReasoning(&json!({ "turn_id": 404 }), &c).unwrap_err();
        assert!(err.contains("interviewer_turn id=404 not found"));
    }
}
