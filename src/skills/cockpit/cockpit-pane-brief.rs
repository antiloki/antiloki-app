//! cockpit-pane-brief.rs — twin of skills/cockpit/cockpit-pane-brief.mjs. NAMES ARE CONTRACT:
//! `cockpitPaneBrief` (default). The PROMPT WINDOW — assembles the EXACT context slice
//! `cockpit-pane-message` would send for one pane (persona, mission + resolved context docs,
//! coordination, scratch, scope, in-scope files) WITHOUT calling any AI, plus per-section token
//! estimates + a total.
//!
//! BESPOKE-NATIVE, not conform-db: dispatches to `gatherScopedFiles` (a real fs directory walk,
//! already twinned in `cockpit_cockpit_pane_message`), same class as that module itself. Every
//! other piece is a pure reuse of already-converted twins: `entities_agent_persona_resolve
//! ::resolvePersona`, `cockpit_cockpit_blackboard_read::cockpitBlackboardRead`,
//! `cockpit__helpers_context_brief::{resolveContextDocs, missionValue, parseExcludes, estTokens}`.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } }
fn truthy_num(n: f64) -> bool { n != 0.0 && !n.is_nan() }
fn num_param(n: f64) -> Value { if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) } }
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }
// JSON.parse(s ?? '[]') catch → [] ; non-array → []
fn parse_arr(s: Option<&Value>) -> Vec<Value> {
    let raw = match s { Some(Value::String(t)) => t.clone(), None | Some(Value::Null) => "[]".to_string(), Some(o) => o.to_string() };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Array(a)) => a,
        _ => Vec::new(),
    }
}

pub fn cockpitPaneBrief(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));
    let pane_id_n = js_number(input.get("pane_id"));
    if !truthy_num(pane_id_n) { return Err("cockpit-pane-brief: pane_id is required".into()); }
    let pane_id = num_param(pane_id_n);

    let rows = crate::db_json::query_rows(db, "SELECT * FROM cockpit_panes WHERE id = ?", &[pane_id.clone()])?;
    let pane = match rows.as_array().and_then(|a| a.first()) {
        Some(p) => p.clone(),
        None => return Ok(json!({ "ok": false, "error": format!("pane {} not found", pane_id_n as i64) })),
    };

    // CLI ENGINES (2026-09-16, operator: "every pane agent should have a button to see the history of
    // prompts and the AI call window with everything that was delivered"): a Claude/Codex/OpenCode
    // pane is not prompted by cockpit-pane-message — what reaches its model is the launch (system
    // prompt addendum + flags), the brief block on disk, the pack, the skills index, the live tools,
    // and the prompts the operator typed. Those are the sections; the engine's own usage rides along.
    if crate::cockpit_cockpit_pane_context_bake::CODE_AGENT_KINDS.contains(&pane.get("pane_kind").and_then(Value::as_str).unwrap_or("")) {
        return Ok(cli_brief(&pane, db));
    }
    let base_agent = pane.get("base_agent").and_then(Value::as_str).unwrap_or("");
    let mut persona = String::new();
    if !base_agent.is_empty() {
        let r = crate::entities_agent_persona_resolve::resolvePersona(db, base_agent);
        if let Some(sm) = r.get("system_md").and_then(Value::as_str) {
            if !sm.is_empty() { persona = sm.to_string(); }
        }
    }
    let extra = str_qq(pane.get("extra_prompt_md"));
    if !extra.trim().is_empty() {
        persona = if persona.is_empty() { format!("OPERATOR EXTRA GUIDANCE:\n{}", extra.trim()) } else { format!("{persona}\n\nOPERATOR EXTRA GUIDANCE:\n{}", extra.trim()) };
    }
    if persona.is_empty() { persona = "(a focused implementation agent working within its scope)".to_string(); }
    let persona = take_chars(&persona, 8000);

    let session_id = pane.get("session_id").cloned().unwrap_or(Value::Null);
    let pane_id_v = pane.get("id").cloned().unwrap_or(Value::Null);
    let slices = crate::cockpit_cockpit_blackboard_read::cockpitBlackboardRead(&json!({ "session_id": session_id, "pane_id": pane_id_v }), db)?;
    let excludes = crate::cockpit__helpers_context_brief::parseExcludes(&pane.get("context_excludes_json").cloned().unwrap_or(Value::Null));
    let resolved_docs = crate::cockpit__helpers_context_brief::resolveContextDocs(db, slices.get("context_docs").unwrap_or(&json!([])), &excludes, &json!({ "db": Value::Null }));
    let mission = crate::cockpit__helpers_context_brief::missionValue(slices.get("mission_md").unwrap_or(&Value::Null), &resolved_docs);

    let coordination = slices.get("coordination").and_then(Value::as_array).map(|a| {
        a.iter().map(|c| format!("- [{}] {}", str_qq(c.get("kind")), str_qq(c.get("body_md")))).collect::<Vec<_>>().join("\n")
    }).unwrap_or_default();
    let coordination = if coordination.is_empty() { "(none)".to_string() } else { take_chars(&coordination, 8000) };

    let scratch = slices.get("scratch").and_then(Value::as_array).map(|a| {
        a.iter().map(|c| format!("- {}", str_qq(c.get("body_md")))).collect::<Vec<_>>().join("\n")
    }).unwrap_or_default();
    let scratch = if scratch.is_empty() { "(none)".to_string() } else { take_chars(&scratch, 4000) };

    let scope = parse_arr(pane.get("scope_json"));
    let scope_text = if scope.is_empty() { "(unrestricted)".to_string() } else { scope.iter().map(|s| str_qq(Some(s))).collect::<Vec<_>>().join("\n") };

    let worktree = pane.get("worktree_path").and_then(Value::as_str).filter(|s| !s.is_empty());
    let files = match worktree {
        Some(w) => str_qq(Some(&crate::cockpit_cockpit_pane_message::gatherScopedFiles(&json!(w), &json!(scope), &Value::Null))),
        None => "(worktree not provisioned)".to_string(),
    };

    let context_pack = worktree.and_then(|w| crate::cockpit_cockpit_pane_context_bake::pack_prompt_text(w, crate::cockpit_cockpit_pane_context_bake::PACK_PROMPT_CAP)).unwrap_or_else(|| "(no context pack baked for this pane)".to_string());

    let section_specs: [(&str, &str, String); 7] = [
        ("persona", "persona · base identity + your guidance", persona),
        ("mission", "mission + shared context (sources embedded)", str_qq(Some(&mission))),
        ("coordination", "coordination · what the agents wrote each other", coordination),
        ("scratch", "scratch · this pane's private memory", scratch),
        ("scope", "edit scope · what it may WRITE", scope_text),
        ("files", "in-scope files · read into the prompt", files),
        ("context_pack", "context pack · antiloki's measured view (reviews, laws, graph, tests)", context_pack),
    ];
    let mut total_tokens: f64 = 0.0;
    let sections: Vec<Value> = section_specs.into_iter().map(|(key, label, body)| {
        let tokens = crate::cockpit__helpers_context_brief::estTokens(&json!(body));
        total_tokens += tokens.as_f64().unwrap_or(0.0);
        json!({ "key": key, "label": label, "body": body, "tokens": tokens })
    }).collect();

    let docs_out: Vec<Value> = resolved_docs.as_array().map(|arr| arr.iter().map(|d| {
        json!({
            "id": d.get("id").cloned().unwrap_or(Value::Null),
            "label": d.get("label").cloned().unwrap_or(Value::Null),
            "source": d.get("source").cloned().unwrap_or(Value::Null),
            "tokens": d.get("tokens").cloned().unwrap_or(Value::Null),
            "excluded": d.get("excluded").cloned().unwrap_or(Value::Null),
        })
    }).collect()).unwrap_or_default();

    let mut pane_out = Map::new();
    for k in ["id", "idx", "subagent_name", "pane_kind", "base_agent", "tier", "model_id"] {
        pane_out.insert(k.into(), pane.get(k).cloned().unwrap_or(Value::Null));
    }

    Ok(json!({
        "ok": true,
        "pane": Value::Object(pane_out),
        "sections": sections,
        "docs": docs_out,
        "totals": { "tokens": num_param(total_tokens) },
    }))
}

fn read_or(p: &std::path::Path, missing: &str) -> String { std::fs::read_to_string(p).unwrap_or_else(|_| missing.to_string()) }
fn block_between(text: &str, a: &str, b: &str) -> Option<String> {
    let s = text.find(a)?; let e = text[s..].find(b)? + s;
    Some(text[s..e + b.len()].to_string())
}

/// The delivery for a CLI engine pane — see the branch in `cockpitPaneBrief`.
fn cli_brief(pane: &Value, db: &Connection) -> Value {
    use crate::cockpit__helpers_context_brief::estTokens;
    let kind = str_qq(pane.get("pane_kind"));
    let wt_str = str_qq(pane.get("worktree_path"));
    let wt = std::path::Path::new(&wt_str);
    let has_wt = !wt_str.is_empty() && wt.is_dir();
    let antiloki = wt.join(".antiloki");
    let mut sections: Vec<Value> = Vec::new();
    let mut total: f64 = 0.0;
    let mut push = |key: &str, label: &str, body: String| {
        let tokens = estTokens(&json!(body));
        total += tokens.as_f64().unwrap_or(0.0);
        sections.push(json!({ "key": key, "label": label, "body": body, "tokens": tokens }));
    };
    // 1. the launch: what pty/start put on the command line (system prompt addendum, mcp config, codex -c)
    let launch: Value = std::fs::read_to_string(antiloki.join("launch.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
    let launch_body = if launch.is_null() { "(not launched through antiloki yet — the CLI has not been started from this pane)".to_string() } else {
        let args = launch.get("args").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut lines = vec![format!("$ {} {}", str_qq(launch.get("cmd")), args.iter().map(|a| { let s = str_qq(Some(a)); if s.contains(' ') { format!("\"{}\"", take_chars(&s, 80).replace('\n', " ")) } else { s } }).collect::<Vec<_>>().join(" "))];
        if let Some(sp) = launch.get("system_prompt").and_then(Value::as_str) { lines.push(String::new()); lines.push("SYSTEM PROMPT ADDENDUM (--append-system-prompt):".into()); lines.push(sp.to_string()); }
        if let Some(env) = launch.get("env").and_then(Value::as_object) { lines.push(String::new()); lines.push(format!("ENV: {}", env.keys().cloned().collect::<Vec<_>>().join(", "))); }
        lines.push(format!("started {}", str_qq(launch.get("t"))));
        lines.join("\n")
    };
    push("launch", "launch · command line + system prompt addendum", launch_body);
    // 2. the brief block the engine reads natively
    let brief_file = if kind == "claude-code" { "CLAUDE.md" } else { "AGENTS.md" };
    let brief_text = if has_wt { read_or(&wt.join(brief_file), "") } else { String::new() };
    let brief_block = block_between(&brief_text, crate::cockpit__helpers_pane_brief_md::BRIEF_A, crate::cockpit__helpers_pane_brief_md::BRIEF_B).unwrap_or_else(|| "(no PANE BRIEF block on disk — the worktree is pending or the brief was never written)".into());
    push("brief", &format!("brief · the PANE BRIEF block in {brief_file} (read natively at start)"), brief_block);
    // 3. the pack: INDEX + every file, exactly as written
    let manifest: Value = std::fs::read_to_string(antiloki.join("context").join("manifest.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
    if manifest.is_null() {
        push("pack", "context pack · nothing baked", "(no context pack for this pane)".into());
    } else {
        push("pack_index", "context pack · INDEX.md", read_or(&antiloki.join("context").join("INDEX.md"), ""));
        for it in manifest.get("items").and_then(Value::as_array).cloned().unwrap_or_default() {
            if !it.get("written").and_then(Value::as_bool).unwrap_or(false) { continue; }
            let f = str_qq(it.get("file"));
            push(&format!("pack:{f}"), &format!("context pack · {f}"), read_or(&antiloki.join("context").join(&f), ""));
        }
    }
    // 4. the skills index + the antiloki-context skill
    let agents_text = if has_wt { read_or(&wt.join("AGENTS.md"), "") } else { String::new() };
    if let Some(idx) = block_between(&agents_text, "<!-- ANTILOKI-SKILLS-INDEX -->", "<!-- /ANTILOKI-SKILLS-INDEX -->") { push("skills", "skills · the index in AGENTS.md (skills load on demand)", idx); }
    let skill = if has_wt { std::fs::read_to_string(wt.join(".agents").join("skills").join("antiloki-context").join("SKILL.md")).ok() } else { None };
    if let Some(sk) = skill { push("skill_context", "skills · antiloki-context/SKILL.md", sk); }
    // 5. the live tools + the hooks
    let mcp: Value = std::fs::read_to_string(antiloki.join("mcp.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
    let tools_body = if mcp.is_null() { "(no live tools for this pane)".to_string() } else {
        let mut lines = vec!["MCP server `antiloki` (stdio):".to_string()];
        for (n, d) in crate::cockpit__helpers_pane_brief_md::MCP_TOOLS { lines.push(format!("- {n} — {d}")); }
        lines.push(String::new());
        lines.push(serde_json::to_string_pretty(&mcp).unwrap_or_default());
        lines.join("\n")
    };
    push("tools", "live tools · MCP", tools_body);
    let hooks_path = match kind.as_str() { "claude-code" => wt.join(".claude").join("settings.local.json"), "codex" => wt.join(".codex").join("hooks.json"), _ => wt.join(".opencode").join("plugins").join("antiloki-context.js") };
    if has_wt { if let Ok(h) = std::fs::read_to_string(&hooks_path) { push("hooks", &format!("hooks · {} (nudge · reads · scope enforcement{})", hooks_path.strip_prefix(wt).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(), if kind == "claude-code" { " · status line" } else { "" }), h); } }
    // the prompt history the hook/plugin logged, newest first
    let prompts: Vec<Value> = std::fs::read_to_string(antiloki.join("prompts.jsonl")).unwrap_or_default().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).map(|mut p| {
        let tokens = estTokens(&json!(str_qq(p.get("prompt"))));
        if let Some(o) = p.as_object_mut() { o.insert("tokens".into(), tokens); }
        p
    }).collect::<Vec<_>>().into_iter().rev().collect();
    let reads = if has_wt { crate::cockpit_cockpit_pane_context_bake::reads_summary(wt) } else { json!({ "total": 0 }) };
    let now = crate::clock::now_iso(db);
    let usage = if !has_wt { Value::Null } else { match kind.as_str() {
        "claude-code" => crate::cockpit__helpers_engine_usage::claude_usage(wt, pane.get("id").and_then(Value::as_i64).unwrap_or(0), &now),
        "codex" => crate::cockpit__helpers_engine_usage::codex_usage(wt, &now),
        _ => crate::cockpit__helpers_engine_usage::opencode_usage(wt, Some(json!({ "cost_usd": pane.get("cost_usd").cloned().unwrap_or(json!(0)), "calls": pane.get("calls").cloned().unwrap_or(json!(0)) })), &now),
    } };
    let mut pane_out = Map::new();
    for k in ["id", "idx", "subagent_name", "pane_kind", "base_agent", "tier", "model_id", "worktree_path"] {
        pane_out.insert(k.into(), pane.get(k).cloned().unwrap_or(Value::Null));
    }
    json!({
        "ok": true,
        "engine": kind,
        "pane": Value::Object(pane_out),
        "sections": sections,
        "docs": [],
        "prompts": prompts,
        "reads": reads,
        "usage": usage,
        "totals": { "tokens": num_param(total) },
        "note": "a CLI engine is not prompted by the cockpit: this is everything antiloki put in front of it (launch flags, the brief block, the pack files, skills, tools, hooks) plus the prompts typed into the pane. The engine's own system prompt and tool schemas are not included.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c.execute_batch(
            // `agents` is core schema, not cockpit schema — ensureCockpitSchema doesn't create it
            // (see cockpit-schema.rs's own test asserting its exact table list). resolvePersona
            // reads name/context_json/custom_prompt_md/specialization_md/parent_agent/is_abstract.
            "CREATE TABLE agents (name TEXT, description TEXT, context_json TEXT, custom_prompt_md TEXT, specialization_md TEXT, parent_agent TEXT, is_abstract INTEGER, created_at TEXT);
             INSERT INTO agents (name, custom_prompt_md, created_at) VALUES ('dev', 'You are dev.', '2025-12-01 00:00:00');
             INSERT INTO cockpit_sessions (id,name,mission_md,created_at) VALUES (1,'s1','Ship it','2025-12-01 00:00:00');
             INSERT INTO cockpit_panes (id, session_id, idx, subagent_name, base_agent, extra_prompt_md, worktree_path, scope_json, grants_json, auto_apply, pane_kind, tier, model_id) \
               VALUES (5, 1, 0, 'alice', 'dev', 'be terse', '', '[]', '[]', 0, 'agent', 'low', NULL);
             INSERT INTO cockpit_blackboard (id,session_id,pane_id,kind,body_md,created_at) VALUES \
               (1,1,NULL,'note','coord one','2026-01-01 09:00:00'), \
               (2,1,5,'scratch','my note','2026-01-01 09:01:00');",
        ).unwrap();
        c
    }

    #[test]
    fn assembles_all_sections() {
        let c = seeded();
        let r = cockpitPaneBrief(&json!({ "pane_id": 5 }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["pane"]["subagent_name"], json!("alice"));
        let sections = r["sections"].as_array().unwrap();
        assert_eq!(sections.len(), 7);
        let by_key = |k: &str| sections.iter().find(|s| s["key"] == json!(k)).unwrap();
        assert!(by_key("persona")["body"].as_str().unwrap().contains("You are dev."));
        assert!(by_key("persona")["body"].as_str().unwrap().contains("OPERATOR EXTRA GUIDANCE"));
        assert!(by_key("persona")["body"].as_str().unwrap().contains("be terse"));
        assert_eq!(by_key("mission")["body"], json!("Ship it"));
        assert_eq!(by_key("coordination")["body"], json!("- [note] coord one"));
        assert_eq!(by_key("scratch")["body"], json!("- my note"));
        assert_eq!(by_key("scope")["body"], json!("(unrestricted)"));
        assert_eq!(by_key("files")["body"], json!("(worktree not provisioned)"));
        let total: f64 = sections.iter().map(|s| s["tokens"].as_f64().unwrap()).sum();
        assert_eq!(r["totals"]["tokens"].as_f64().unwrap(), total);
    }

    #[test]
    fn pane_not_found_returns_ok_false() {
        let c = seeded();
        let r = cockpitPaneBrief(&json!({ "pane_id": 999 }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("pane 999 not found"));
    }

    #[test]
    fn missing_pane_id_errors() {
        let c = seeded();
        let e = cockpitPaneBrief(&json!({}), &c).unwrap_err();
        assert!(e.contains("pane_id is required"));
    }

    #[test]
    fn cli_pane_brief_is_the_delivery_not_the_agent_prompt() {
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        let wt = std::env::temp_dir().join(format!("antiloki-cli-brief-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&wt);
        std::fs::create_dir_all(wt.join(".antiloki/context/files")).unwrap();
        std::fs::write(wt.join("CLAUDE.md"), "<!-- ANTILOKI-PANE-BRIEF -->\n# PANE BRIEF\nSCOPE: src/**\n<!-- /ANTILOKI-PANE-BRIEF -->\n\n# repo\n").unwrap();
        std::fs::write(wt.join(".antiloki/launch.json"), r#"{"cmd":"claude","args":["--append-system-prompt","read the pack first","--mcp-config",".antiloki/mcp.json"],"system_prompt":"read the pack first","env":{"ANTILOKI_PANE_ID":"1"},"t":"2026-09-16T10:00:00Z"}"#).unwrap();
        std::fs::write(wt.join(".antiloki/context/manifest.json"), r#"{"items":[{"kind":"laws","file":"laws.md","written":true},{"kind":"file","path":"src/a.ts","file":"files/src__a.ts.md","written":false}]}"#).unwrap();
        std::fs::write(wt.join(".antiloki/context/INDEX.md"), "# Your context pack\n").unwrap();
        std::fs::write(wt.join(".antiloki/context/laws.md"), "# laws\n").unwrap();
        std::fs::write(wt.join(".antiloki/mcp.json"), "{\"mcpServers\":{}}").unwrap();
        std::fs::write(wt.join(".antiloki/prompts.jsonl"), "{\"t\":\"1\",\"prompt\":\"first\"}\n{\"t\":\"2\",\"prompt\":\"second one\"}\n").unwrap();
        c.execute("INSERT INTO cockpit_panes (id, session_id, idx, subagent_name, pane_kind, worktree_path, scope_json) VALUES (1, 1, 0, 'cc', 'claude-code', ?1, '[\"src/**\"]')", [wt.to_string_lossy().to_string()]).unwrap();
        let r = cockpitPaneBrief(&json!({ "pane_id": 1 }), &c).unwrap();
        assert_eq!(r["engine"], json!("claude-code"));
        let keys: Vec<&str> = r["sections"].as_array().unwrap().iter().map(|s| s["key"].as_str().unwrap()).collect();
        assert_eq!(keys, vec!["launch", "brief", "pack_index", "pack:laws.md", "tools"], "{keys:?} — the unwritten pack item is not a section");
        assert!(r["sections"][0]["body"].as_str().unwrap().contains("SYSTEM PROMPT ADDENDUM (--append-system-prompt):\nread the pack first"));
        assert!(r["sections"][1]["body"].as_str().unwrap().starts_with("<!-- ANTILOKI-PANE-BRIEF -->"));
        assert_eq!(r["prompts"][0]["prompt"], json!("second one"), "newest first");
        assert_eq!(r["prompts"][0]["tokens"], json!(3));
        assert!(r["totals"]["tokens"].as_i64().unwrap() > 0);
        assert_eq!(r["usage"]["source"], json!("none"));
        let _ = std::fs::remove_dir_all(&wt);
    }
}
