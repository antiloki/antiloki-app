//! atlas-architecture-diagram.rs — twin of skills/codebase/atlas-architecture-diagram.mjs (db lane,
//! cross-db brain.* via the prompt-find/prompt-add twins). NAMES ARE CONTRACT: default
//! `atlasArchitectureDiagram`. Authors a SEMANTIC architecture/data-flow diagram (nodes with
//! roles + labeled edges) GROUNDED in a set of source files, for the chat's "explain the
//! architecture" answer — a deterministic layout + renderer (FE) draws it; the model never
//! positions anything.
//!
//! `context.db` missing → JS returns `{ ok:false, error:'no db' }` (NOT a throw) — ported as the
//! same early return here since `db: &Connection` can't literally be absent in Rust, but the
//! `files` guard right after it (`{ ok:false, error:'no files' }`) IS reachable and ported.
//! `context.provider_fn` (a JS function reference for provider selection/DI) has no Rust
//! equivalent to carry — threaded as `null` into the constructed aiCall input, same convention as
//! book-run.rs's/pl-research.rs's `provider_fn`.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`, so
//! `atlasArchitectureDiagram` propagates that error once the (real, ported) deterministic surface
//! above it has run: the `files`/`ask` guards, the `filesBlock` assembly (path + up-to-2500-char
//! source per file), and `ensure_prompt` (reusing the already-converted `ai_prompt_find`/
//! `ai_prompt_add` twins, mirroring book-run.rs's `ensure_faculty` / pl-research.rs's
//! `ensure_prompt`). The response-shaping logic that runs AFTER a real `aiCall` (JSON.parse of
//! `diagram_json`, node/edge filtering+capping, `telemetryFromResult`) is unreachable from this
//! file's own fixture (the `?` on `aiCall` short-circuits first) but is ported faithfully anyway
//! per the crossing playbook's coverage rule; `telemetryFromResult` itself is exercised by its own
//! twin's tests (`ai/_helpers/ai-telemetry.rs`), reused here verbatim.
// QUALITY: mixed unit — deterministic guard/assembly/prompt-ensure surface (fully ported,
// fixture-reachable up to the deferred aiCall boundary) + 1 bespoke/deferred aiCall dispatch (no
// fixture/frontier).
use rusqlite::Connection;
use serde_json::{json, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
} // = JS String(v ?? '')
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
} // = JS String(v)
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

const PROMPT: &str = "atlas.architecture.diagram.v3";
const SYSTEM: &str = "You explain software architecture as a clear diagram a developer reads at a glance. You output only strict JSON. Produce a SEMANTIC node/edge graph (you do NOT position anything — a layout engine does that). Each node is a real part of the system (a module / component / store / service / external dependency) with a short label, a role, and the FILE PATH it maps to (\"file\" — exactly one of the provided paths, or \"\" for an external dependency). Each edge is a real relationship or data-flow with a SHORT label saying what flows or what the call does. Ground everything in the provided source; invent nothing. 4-9 nodes — the essential architecture + data flow, not every detail. Also write a CAPTION: 2-3 plain sentences (markdown OK) that READ ABOVE the diagram — explain what the diagram shows and, where relevant, how the proposed improvements change this flow. The caption is prose the reader sees before the boxes, so it must stand on its own.";
const TEMPLATE: &str = "Explain the architecture and data-flow of these files as a diagram a developer can read at a glance. Identify the key parts and how data / control flows between them (left-to-right or producer→consumer). Then write a short caption explaining it (and how the suggested changes affect it, if any).\n\n{{ask}}\n\nTHE FILES:\n{{files}}\n\nReturn JSON only, exactly:\n{\"title\":\"<short title of what this shows>\",\"caption\":\"<2-3 sentences explaining the flow and how the proposed changes affect it>\",\"nodes\":[{\"id\":\"<short unique id>\",\"label\":\"<2-4 word name>\",\"role\":\"ui|state|logic|data|external\",\"file\":\"<file path this maps to, or empty for external>\"}],\"edges\":[{\"from\":\"<node id>\",\"to\":\"<node id>\",\"label\":\"<what flows: e.g. 'query', 'results', 'dispatch', 'reads'>\"}]}";

fn prompt_output_fields() -> Value {
    json!([{ "field_name": "diagram_json", "field_type": "string", "required": true, "min_length": 10 }])
}

// mirrors JS ensurePrompt: try promptFind; if missing OR its template_md differs from TEMPLATE,
// promptAdd (a fresh version) — no re-find after add (JS doesn't re-find here either; the fresh
// row is used implicitly by aiCall's own lookup-by-name-latest-version on its next call).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(row) = &existing {
        if row.get("template_md").and_then(Value::as_str) == Some(TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "mid",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 1700,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    Ok(())
}

// `files.map(f => `=== ${f.path} ===\n${String(f.source ?? '').slice(0, 2500)}`).join('\n\n')`
fn files_block(files: &[Value]) -> String {
    files
        .iter()
        .map(|f| {
            let path = js_string(f.get("path").unwrap_or(&Value::Null));
            let source: String = str_qq(f.get("source")).chars().take(2500).collect();
            format!("=== {path} ===\n{source}")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

// Pure — decomposes a diagram_json parse into the capped/filtered `{nodes, edges}` shape.
// Ported for coverage (crossing playbook rule) though unreachable from the default export itself
// (the `?` on aiCall short-circuits before a real diagram_json ever exists).
fn shape_diagram(d: &Value) -> Result<Value, String> {
    let nodes: Vec<Value> = d
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|n| n.is_object() && !n.get("id").map(Value::is_null).unwrap_or(true))
        .take(12)
        .map(|n| {
            let id = js_string(n.get("id").unwrap_or(&Value::Null));
            let label = n.get("label").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| id.clone());
            let role = n.get("role").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "logic".to_string());
            let file = n.get("file").filter(|v| truthy(Some(v))).map(js_string);
            json!({ "id": id, "label": label, "role": role, "file": file })
        })
        .collect();
    if nodes.len() < 2 {
        return Err("diagram too small".to_string());
    }
    let ids: std::collections::HashSet<String> = nodes.iter().map(|n| n["id"].as_str().unwrap_or("").to_string()).collect();
    let edges: Vec<Value> = d
        .get("edges")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| {
            if !e.is_object() { return false; }
            let from = js_string(e.get("from").unwrap_or(&Value::Null));
            let to = js_string(e.get("to").unwrap_or(&Value::Null));
            ids.contains(&from) && ids.contains(&to) && from != to
        })
        .take(24)
        .map(|e| {
            let from = js_string(e.get("from").unwrap_or(&Value::Null));
            let to = js_string(e.get("to").unwrap_or(&Value::Null));
            let label = e.get("label").filter(|v| truthy(Some(v))).map(js_string).unwrap_or_default();
            json!({ "from": from, "to": to, "label": label })
        })
        .collect();
    let title = d.get("title").filter(|v| truthy(Some(v))).map(js_string);
    let caption = d.get("caption").filter(|v| truthy(Some(v))).map(js_string);
    Ok(json!({ "title": title, "caption": caption, "nodes": nodes, "edges": edges }))
}

pub fn atlasArchitectureDiagram(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) return { ok:false, error:'no db' };`
    // — unreachable here (Rust's `db: &Connection` is never absent).
    let provider_fn = Value::Null;
    let files: Vec<Value> = input.get("files").and_then(Value::as_array).cloned().unwrap_or_default();
    if files.is_empty() {
        return Ok(json!({ "ok": false, "error": "no files" }));
    }
    // `String(input?.ask ?? '').slice(0, 400)`
    let ask: String = str_qq(input.get("ask")).chars().take(400).collect();
    let block = files_block(&files);

    ensure_prompt(db)?;

    // ── the AI call: PARITY-DEFERRED from here on (see module doc) — aiCall always returns Err.
    let ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [
            { "key": "ask", "value": ask },
            { "key": "files", "value": block },
        ],
        "tier": "mid",
        "temperature": "cold",
        "phase_flow": "single_only",
        "provider_fn": provider_fn,
    });
    let res = crate::ai_ai_call::aiCall(&ai_input, db)?;

    let raw = res
        .get("output_kv")
        .and_then(Value::as_array)
        .and_then(|arr| {
            arr.iter().find(|o| {
                o.get("field_name").and_then(Value::as_str) == Some("diagram_json")
                    && o.get("parent_output_id").map(Value::is_null).unwrap_or(true)
            })
        })
        .and_then(|o| o.get("value_text"))
        .filter(|v| !v.is_null());
    let Some(raw) = raw else {
        return Ok(json!({ "ok": false, "error": "no diagram returned" }));
    };
    let parsed: Value = match serde_json::from_str(&str_qq(Some(raw))) {
        Ok(v) => v,
        Err(_) => return Ok(json!({ "ok": false, "error": "diagram parse failed" })),
    };
    let shaped = match shape_diagram(&parsed) {
        Ok(v) => v,
        Err(e) => return Ok(json!({ "ok": false, "error": e })),
    };
    let telemetry = crate::ai__helpers_ai_telemetry::telemetryFromResult(&res, db, Some(&json!("diagram")), Some(&json!(PROMPT)));
    Ok(json!({ "ok": true, "diagram": shaped, "ai_telemetry": telemetry }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn no_files_short_circuits_ok_false() {
        let c = setup();
        let r = atlasArchitectureDiagram(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "no files" }));
        let r2 = atlasArchitectureDiagram(&json!({ "files": [] }), &c).unwrap();
        assert_eq!(r2, json!({ "ok": false, "error": "no files" }));
    }

    #[test]
    fn files_block_joins_path_and_capped_source() {
        let files = vec![
            json!({ "path": "a.mjs", "source": "const x = 1;" }),
            json!({ "path": "b.mjs", "source": "x".repeat(3000) }),
        ];
        let block = files_block(&files);
        assert!(block.starts_with("=== a.mjs ===\nconst x = 1;\n\n=== b.mjs ===\n"));
        let b_source = block.split("=== b.mjs ===\n").nth(1).unwrap();
        assert_eq!(b_source.chars().count(), 2500);
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_after_full_deterministic_surface() {
        let c = setup();
        let files = vec![json!({ "path": "a.mjs", "source": "export default function a(){}" })];
        let err = atlasArchitectureDiagram(&json!({ "files": files, "ask": "why?" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'atlas.architecture.diagram.v3'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_prompt_reuses_matching_template_without_reinserting() {
        let c = setup();
        ensure_prompt(&c).unwrap();
        ensure_prompt(&c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'atlas.architecture.diagram.v3'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn shape_diagram_filters_bad_edges_and_caps_nodes() {
        let d = json!({
            "title": "Flow",
            "caption": "the flow",
            "nodes": [
                { "id": "a", "label": "A", "role": "ui", "file": "a.mjs" },
                { "id": "b", "label": "B", "role": "logic", "file": "" },
            ],
            "edges": [
                { "from": "a", "to": "b", "label": "calls" },
                { "from": "a", "to": "missing", "label": "bad" },
                { "from": "a", "to": "a", "label": "self" },
            ],
        });
        let out = shape_diagram(&d).unwrap();
        assert_eq!(out["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(out["nodes"][1]["file"], Value::Null); // empty string file -> null (not truthy)
        assert_eq!(out["edges"].as_array().unwrap().len(), 1);
        assert_eq!(out["edges"][0]["label"], json!("calls"));
        assert_eq!(out["title"], json!("Flow"));
    }

    #[test]
    fn shape_diagram_errors_when_fewer_than_two_nodes_survive() {
        let d = json!({ "nodes": [{ "id": "a" }], "edges": [] });
        assert_eq!(shape_diagram(&d).unwrap_err(), "diagram too small");
    }
}
