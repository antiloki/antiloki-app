//! author-blocks.rs — twin of skills/swarm/author-blocks.mjs (db, cross-db brain.* via the
//! prompt-find/prompt-add twins). NAMES ARE CONTRACT: default `authorBlocks`.
//!
//! The LOW per-file author swarm (the "shell"): for each block in the contract's active
//! manifest, author the file from that block's binding-resolved SLICE (role + exactly what it
//! must export + what it imports and from where), then gate the whole set with the deterministic
//! composition-check. Each authored file lands in swarm_blocks.
//!
//! PARITY-DEFERRED: each block's authoring call ends in `crate::ai_ai_call::aiCall` (the deferred
//! network-AI stub — see ai-call.rs's module doc), which always returns `Err`. The JS `await
//! aiCall(...)` is UNGUARDED (no try/catch around it) inside the accept-retry loop, so a rejection
//! propagates straight out of `authorBlocks` — this twin mirrors that with `?`. Everything up to
//! and including that first call is ported faithfully and IS the deterministic surface these tests
//! exercise: the guards, the active-manifest lookup + parse, `ensure_author_prompt` (reusing the
//! already-converted `ai_prompt_find`/`ai_prompt_add` twins), the per-block slice resolution
//! (`swarm_manifest_lint::resolveBlockSlice`), the deterministic subagent routing
//! (`swarm_subagent_route::routeFile`) + persona resolution (`entities_agent_persona_resolve::resolvePersona`),
//! and the pure `render_produces`/`render_consumes`/`unfence` prompt-assembly helpers (unit-tested
//! directly). The accept-retry loop's `acceptAuthoredCode` call, the `swarm_blocks` upsert, and the
//! whole-set `checkComposition` gate + `contract_qa_reviews` insert are ported byte-for-byte from
//! the JS but are unreachable by this file's own fixture since they run only after a successful
//! `aiCall`, which the deferred stub never produces — correctness there is by inspection, mirroring
//! the same convention as book-run.rs / pl-research.rs / file-summarize.rs.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
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

const AUTHOR_PROMPT: &str = "swarm.author.block.v1";
const AUTHOR_TEMPLATE: &str = "Author the COMPLETE contents of ONE file. Output ONLY the file's content (no prose, no explanation, no code fences) into the file_content field.\n\nFILE: {{file}}\nROLE: {{role}}\n\nThis file MUST EXPORT exactly these symbols (other files in the blueprint depend on them):\n{{produces}}\n\nThis file CONSUMES these inputs — import each from the named source:\n{{consumes}}\n\nWrite idiomatic, complete, working code. Export EXACTLY the named symbols above. Import each consumed symbol from its named source file. Do not add files or exports that were not asked for.";

// try promptFind; if found AND its template_md matches verbatim, no-op. Else (miss OR stale)
// promptAdd a fresh version. Mirrors JS's try/catch around promptFind (miss → null, not thrown).
fn ensure_author_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": AUTHOR_PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(|v| v.as_str()) == Some(AUTHOR_TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": AUTHOR_PROMPT,
            "template_md": AUTHOR_TEMPLATE,
            "default_tier": "low",
            "default_temperature": "freezed",
            "phase_flow": "single_only",
            "max_output_tokens": 2000,
            "output_fields": [{ "field_name": "file_content", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

// `res?.output_kv?.find(o => o.field_name === 'file_content' && o.parent_output_id == null)?.value_text ?? null`
fn pick(res: Option<&Value>) -> Option<Value> {
    let res = res?;
    let kv = res.get("output_kv")?.as_array()?;
    let found = kv.iter().find(|o| {
        o.get("field_name").and_then(|v| v.as_str()) == Some("file_content")
            && !truthy(o.get("parent_output_id")) // `== null` — JS loose null check catches undefined too
    })?;
    nz(found.get("value_text")).cloned()
}

// Strip an accidental code fence the low model may wrap the file in.
fn unfence(s: Option<&Value>) -> String {
    let str_val = str_qq(s);
    let re = regex::Regex::new(r"(?s)^\s*```[\w.-]*\n(.*?)\n```\s*$").unwrap();
    match re.captures(&str_val) {
        Some(caps) => caps.get(1).map(|m| m.as_str().to_string()).unwrap_or(str_val),
        None => str_val,
    }
}

fn render_produces(slice: &Value) -> String {
    let ps: Vec<Value> = slice.get("produces").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if ps.is_empty() {
        return "- (nothing — this is a leaf/entry file)".to_string();
    }
    ps.iter()
        .map(|p| {
            let name = tmpl(p.get("name"));
            let ty = p.get("type").filter(|v| truthy(Some(v))).map(|t| format!(" : {}", tmpl(Some(t)))).unwrap_or_default();
            format!("- {name}{ty}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_consumes(slice: &Value) -> String {
    let cs: Vec<Value> = slice.get("consumes").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if cs.is_empty() {
        return "- (nothing)".to_string();
    }
    cs.iter()
        .map(|c| {
            let name = tmpl(c.get("name"));
            let ty = c.get("type").filter(|v| truthy(Some(v))).map(|t| format!(" : {}", tmpl(Some(t)))).unwrap_or_default();
            if truthy(c.get("external")) {
                format!("- {name}{ty} — from an external package/global")
            } else {
                let source = c.get("source");
                let file = source.and_then(|s| s.get("file")).filter(|v| truthy(Some(v))).map(|v| tmpl(Some(v))).unwrap_or_else(|| "?".to_string());
                let port = source.and_then(|s| s.get("port")).filter(|v| truthy(Some(v))).map(|v| tmpl(Some(v))).unwrap_or(name.clone());
                format!("- {name}{ty} — from `{file}` (exported as `{port}`)")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn first(rows: &Value) -> Option<Value> {
    rows.as_array().and_then(|a| a.first()).cloned()
}

pub fn authorBlocks(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent). provider_fn is a JS function
    // reference threaded opaquely into aiCall's input; there is no Rust equivalent to carry (the
    // dispatch itself is the deferred boundary), so it is passed as `null` below.
    let provider_fn = Value::Null;
    let slug = match input.get("contract_slug").filter(|v| truthy(Some(v))) {
        Some(s) => s.clone(),
        None => return Err("author-blocks: contract_slug is required".to_string()),
    };

    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, manifest_json FROM swarm_manifests WHERE contract_slug = ? AND superseded_by IS NULL ORDER BY version DESC, id DESC LIMIT 1",
        &[slug.clone()],
    )?;
    let row = match first(&rows) {
        Some(r) => r,
        None => return Err(format!("author-blocks: active swarm_manifest for contract '{}' not found", tmpl(Some(&slug)))),
    };
    let row_id = row.get("id").cloned().unwrap_or(Value::Null);
    let manifest: Value = row.get("manifest_json").and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or(Value::Null);
    let blocks: Vec<Value> = manifest.get("blocks").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if blocks.is_empty() {
        return Err(format!("author-blocks: blocks in manifest '{}' not found", tmpl(Some(&slug))));
    }

    ensure_author_prompt(db)?;

    let authored_by_input = str_qq(input.get("authored_by").filter(|v| truthy(Some(v)))).chars().collect::<String>();
    let authored_by_input = if authored_by_input.is_empty() { "driver:auto:swarm".to_string() } else { authored_by_input };

    let mut authored: Vec<Value> = Vec::new();
    for block in &blocks {
        let block_id = block.get("id").cloned().unwrap_or(Value::Null);
        let slice = crate::swarm_manifest_lint::resolveBlockSlice(&manifest, &block_id);

        // EPIC-SUBAGENT: mount the specialized authoring LEAF for this file — the manifest's
        // assigned subagent_id if present, else the deterministic router (file-type → leaf,
        // fallback to the tl.dev branch root).
        let block_file = block.get("file").cloned().unwrap_or(Value::Null);
        let subagent_id = match block.get("subagent_id").filter(|v| truthy(Some(v))) {
            Some(v) => v.clone(),
            None => crate::swarm_subagent_route::routeFile(db, &block_file, &json!("tl.dev"), &Value::Null)
                .get("subagent_id")
                .cloned()
                .unwrap_or(Value::Null),
        };
        let subagent_id_str = subagent_id.as_str().unwrap_or("").to_string();
        let resolved_persona = crate::entities_agent_persona_resolve::resolvePersona(db, &subagent_id_str);
        let persona_md: Value = if resolved_persona.is_null() {
            Value::Null
        } else {
            nz(resolved_persona.get("system_md")).cloned().unwrap_or(Value::Null)
        };

        let block_role = str_qq(block.get("role"));
        let input_kv = json!([
            { "key": "file", "value": block_file },
            { "key": "role", "value": block_role },
            { "key": "produces", "value": render_produces(&slice) },
            { "key": "consumes", "value": render_consumes(&slice) },
        ]);

        // ACCEPT RULE — the ONE shared parse-guard (author-accept, 2026-07-09 Class-A class-fix).
        // Accept only if the model's output PARSES, else retry with a rotated model (bypass_cache).
        // PARITY-DEFERRED from here: `aiCall` (called on attempt 0 below) always returns Err, and
        // the JS `await aiCall(...)` is unguarded — so this propagates immediately, matching JS.
        let mut content: Option<String> = None;
        for attempt in 0..3 {
            let mut ai_input = Map::new();
            ai_input.insert("prompt_name".into(), json!(AUTHOR_PROMPT));
            ai_input.insert("input_kv".into(), input_kv.clone());
            ai_input.insert("system_md".into(), persona_md.clone());
            ai_input.insert("subagent".into(), subagent_id.clone());
            ai_input.insert("contract_slug".into(), slug.clone());
            ai_input.insert("tier".into(), json!("low"));
            ai_input.insert("temperature".into(), json!("freezed"));
            ai_input.insert("phase_flow".into(), json!("single_only"));
            ai_input.insert("provider_fn".into(), provider_fn.clone());
            if attempt > 0 {
                ai_input.insert("bypass_cache".into(), json!(true));
            }
            let res = crate::ai_ai_call::aiCall(&Value::Object(ai_input), db)?;
            let picked = pick(Some(&res));
            content = Some(unfence(picked.as_ref())); // keep the latest attempt regardless — the gate is the backstop
            let ai_call_id = res.get("ai_call_id").cloned().unwrap_or(Value::Null);
            let accept = crate::blueprint__helpers_author_accept::acceptAuthoredCode(
                db,
                &json!({ "code": content, "path": block_file, "ai_call_id": ai_call_id }),
            );
            if truthy(accept.get("ok")) {
                break;
            }
        }

        crate::db_json::run_write(
            db,
            "INSERT INTO swarm_blocks (contract_slug, manifest_id, block_id, file, content, authored_by) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(manifest_id, block_id) DO UPDATE SET file=excluded.file, content=excluded.content, authored_by=excluded.authored_by, authored_at=datetime('now')",
            &[
                slug.clone(),
                row_id.clone(),
                block_id.clone(),
                block_file.clone(),
                content.clone().map(|c| json!(c)).unwrap_or(Value::Null),
                json!(format!("{authored_by_input}:{subagent_id_str}")),
            ],
        )?;
        authored.push(json!({ "path": block_file, "content": content, "subagent_id": subagent_id }));
    }

    // Deterministic composition gate over the whole authored set.
    let compose = crate::swarm_composition_check::checkComposition(&manifest, &json!(authored));
    let ok = truthy(compose.get("ok"));
    let mismatches = compose.get("mismatches").cloned().unwrap_or(json!([]));
    crate::db_json::run_write(
        db,
        "UPDATE swarm_blocks SET compose_passed = ?, compose_json = ? WHERE manifest_id = ?",
        &[
            json!(if ok { 1 } else { 0 }),
            json!(serde_json::to_string(&json!({ "mismatches": mismatches, "checked": compose.get("checked").cloned().unwrap_or(Value::Null) })).map_err(|e| e.to_string())?),
            row_id.clone(),
        ],
    )?;
    let findings = if ok {
        format!("Composition GREEN: {} binding(s) realized by the authored exports/imports.", tmpl(compose.get("checked")))
    } else {
        let parts: Vec<String> = mismatches.as_array().unwrap_or(&vec![]).iter()
            .map(|m| format!("{}: {}", tmpl(m.get("kind")), tmpl(m.get("detail"))))
            .collect();
        format!("Composition RED: {} binding(s) not realized — {}", mismatches.as_array().map(|a| a.len()).unwrap_or(0), parts.join("; "))
    };
    let reviewer = match input.get("reviewer").filter(|v| truthy(Some(v))) {
        Some(v) => v.clone(),
        None => json!("driver:auto"),
    };
    crate::db_json::run_write(
        db,
        "INSERT INTO contract_qa_reviews (contract_slug, passed, verdict, findings_md, issues_json, reviewer, failure_target, review_mode) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            slug.clone(),
            json!(if ok { 1 } else { 0 }),
            json!(if ok { "pass" } else { "fail" }),
            json!(findings),
            json!(serde_json::to_string(&mismatches).map_err(|e| e.to_string())?),
            reviewer,
            json!("microtasks"),
            json!("composition"),
        ],
    )?;

    Ok(json!({
        "contract_slug": slug,
        "manifest_id": row_id,
        "blocks_authored": authored.len() as i64,
        "compose_passed": ok,
        "mismatches": mismatches,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE swarm_manifests (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, version INTEGER NOT NULL DEFAULT 1, manifest_json TEXT NOT NULL, superseded_by INTEGER, created_at TEXT);
             CREATE TABLE swarm_blocks (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, manifest_id INTEGER NOT NULL, block_id TEXT NOT NULL, file TEXT, content TEXT, authored_by TEXT, compose_passed INTEGER, compose_json TEXT, authored_at TEXT NOT NULL DEFAULT (datetime('now')), UNIQUE(manifest_id, block_id));
             CREATE TABLE contract_qa_reviews (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, passed INTEGER, verdict TEXT, findings_md TEXT, issues_json TEXT, reviewer TEXT, failure_target TEXT, review_mode TEXT, created_at TEXT NOT NULL DEFAULT (datetime('now')));
             CREATE TABLE agents (name TEXT, description TEXT, context_json TEXT, custom_prompt_md TEXT, specialization_md TEXT, parent_agent TEXT, is_abstract INTEGER, applies_to_json TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    fn a_manifest(c: &Connection, slug: &str, manifest: &Value) -> i64 {
        c.execute(
            "INSERT INTO swarm_manifests (contract_slug, manifest_json) VALUES (?, ?)",
            rusqlite::params![slug, serde_json::to_string(manifest).unwrap()],
        ).unwrap();
        c.last_insert_rowid()
    }

    fn a_manifest_json() -> Value {
        json!({
            "blocks": [
                { "id": "b1", "file": "src/foo.mjs", "role": "does foo stuff" },
            ],
            "bindings": [],
        })
    }

    #[test]
    fn requires_contract_slug() {
        let c = setup();
        assert!(authorBlocks(&json!({}), &c).unwrap_err().contains("contract_slug is required"));
    }

    #[test]
    fn errors_when_no_active_manifest() {
        let c = setup();
        let e = authorBlocks(&json!({ "contract_slug": "ghost" }), &c).unwrap_err();
        assert!(e.contains("active swarm_manifest for contract 'ghost' not found"));
    }

    #[test]
    fn errors_when_manifest_has_no_blocks() {
        let c = setup();
        a_manifest(&c, "empty-slug", &json!({ "blocks": [] }));
        let e = authorBlocks(&json!({ "contract_slug": "empty-slug" }), &c).unwrap_err();
        assert!(e.contains("blocks in manifest 'empty-slug' not found"));
    }

    #[test]
    fn ensures_author_prompt_then_reaches_the_parity_deferred_aicall_boundary() {
        // proves every deterministic gate ran cleanly (manifest lookup, blocks guard,
        // ensure_author_prompt, per-block slice/subagent/persona resolution) and the error is
        // aiCall's deferred stub, not an earlier bug.
        let c = setup();
        a_manifest(&c, "acme", &a_manifest_json());
        let err = authorBlocks(&json!({ "contract_slug": "acme" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'swarm.author.block.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_author_prompt_is_idempotent_when_template_matches() {
        let c = setup();
        ensure_author_prompt(&c).unwrap();
        ensure_author_prompt(&c).unwrap(); // second call sees a matching template_md, no-ops
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'swarm.author.block.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_author_prompt_re_registers_when_template_drifted() {
        let c = setup();
        crate::ai_prompt_add::promptAdd(
            &json!({ "name": AUTHOR_PROMPT, "template_md": "some stale body", "output_fields": [{ "field_name": "file_content", "field_type": "string" }] }),
            &c,
        ).unwrap();
        ensure_author_prompt(&c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'swarm.author.block.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 2); // stale one superseded, fresh one registered
        let active: String = c.query_row("SELECT template_md FROM brain.prompt_templates WHERE name = 'swarm.author.block.v1' AND superseded_by IS NULL", [], |row| row.get(0)).unwrap();
        assert_eq!(active, AUTHOR_TEMPLATE);
    }

    #[test]
    fn render_produces_lists_named_ports_or_leaf_fallback() {
        assert_eq!(render_produces(&json!({})), "- (nothing — this is a leaf/entry file)");
        assert_eq!(render_produces(&json!({ "produces": [] })), "- (nothing — this is a leaf/entry file)");
        let s = render_produces(&json!({ "produces": [{ "name": "foo" }, { "name": "bar", "type": "string" }] }));
        assert_eq!(s, "- foo\n- bar : string");
    }

    #[test]
    fn render_consumes_lists_external_and_internal_sources() {
        assert_eq!(render_consumes(&json!({})), "- (nothing)");
        let s = render_consumes(&json!({
            "consumes": [
                { "name": "lodash", "external": true },
                { "name": "helper", "type": "fn", "source": { "file": "src/lib.mjs", "port": "helper" } },
                { "name": "unresolved" },
            ]
        }));
        assert_eq!(
            s,
            "- lodash — from an external package/global\n- helper : fn — from `src/lib.mjs` (exported as `helper`)\n- unresolved — from `?` (exported as `unresolved`)"
        );
    }

    #[test]
    fn unfence_strips_a_wrapping_code_fence() {
        assert_eq!(unfence(Some(&json!("```js\nconst x = 1;\n```"))), "const x = 1;");
        assert_eq!(unfence(Some(&json!("plain content, no fence"))), "plain content, no fence");
        assert_eq!(unfence(None), "");
    }

    #[test]
    fn pick_extracts_file_content_output_field() {
        let res = json!({ "output_kv": [
            { "field_name": "other", "value_text": "nope" },
            { "field_name": "file_content", "parent_output_id": null, "value_text": "the code" },
        ]});
        assert_eq!(pick(Some(&res)), Some(json!("the code")));
        assert_eq!(pick(Some(&json!({}))), None);
        assert_eq!(pick(None), None);
    }
}
